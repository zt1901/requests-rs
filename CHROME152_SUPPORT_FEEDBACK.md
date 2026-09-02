# Chrome 152 Trust Anchor IDs 验收记录

## 结论

Google Chrome官方正式版`152.0.7977.64`已经作为内置`chrome152`发布。记录ID为`66e05a9d467e4b54b2c2b71f12ced6f7`，来源样本为`browser-fingerprint-collector/chrome152_collector_profile.json`。

原阻塞点TLS扩展`0xca34`（Trust Anchor Identifiers）已端到端接通。实现不会删除未知扩展或把请求成功当作指纹等价；未知非GREASE字段和损坏payload会在Session构造期fail-closed。

## 源记录

| 字段 | 实测值 |
|---|---|
| 产品 | Google Chrome（官方正式版） |
| 完整版本 | `152.0.7977.64` |
| Profile | `chrome152` |
| HTTP协议 | HTTP/2 |
| ClientHello长度 | 1946 |
| TLS Record长度 | 1951 |
| 规范JA4 | `t13i1516h2_8daaf6152771_cb7bf5808d99` |
| `0xca34`payload长度 | 206字节 |
| `0xca34`payload SHA-256 | `c9378cede9834fac982362518778475db2d9c3b3c54092910632ed74ab80ee01` |

`0xca34` payload由2字节外层列表长度和204字节非空8位长度前缀Trust Anchor ID列表组成。实现严格校验外层长度、内部ID边界、唯一扩展记录和Base64；传给BoringSSL API时只传内部列表，避免重复编码外层长度。

## 实现层级

| 层 | 已完成能力 |
|---|---|
| BoringSSL | Trust Anchor配置、复制生命周期、ClientHello扩展编码和公开`set1` API |
| btls | `SSL_CTX_set1_requested_trust_anchors`安全Rust包装 |
| wreq | `TlsOptions`配置、Connector应用及`ExtensionType::TRUST_ANCHORS`扩展顺序 |
| requests_rust | `extension_wire`严格解析、schema校验、payload传递及fail-closed错误 |
| 指纹数据 | `browser_baselines.json`与`fingerprints.json`内置`chrome152`；schema 2补充TLS/QUIC/H3/QPACK完整模板 |
| quiche | QUIC Transport Parameters顺序和值、DCID/SCID与Initial尺寸、HTTP/3 SETTINGS顺序及QPACK策略 |

wreq和匹配的btls wrapper源码随仓库vendor，构建不依赖被本机修改的Cargo缓存。

## 线级证据

`test_collector_contract.py`分别使用捕获器外部JSON和wheel内置`chrome152`建立真实本地TLS连接并解析ClientHello。两条路径均满足：

- 实际发送扩展`0xca34`。
- payload长度为206字节。
- payload与源样本逐字节相同。
- payload SHA-256为`c9378c...ee01`。
- `fingerprint_id`命中源记录。
- 非法Base64、长度错误和未来schema在Session创建时被拒绝。

同时通过Chrome/Edge/Firefox profile、Edge签名GREASE、HTTP/3、Python API和wheel安装回归。没有`0xca34`的旧profile不会发送该扩展。

## 边界

Chrome 152现有schema 2记录同时覆盖HTTP/1.1、HTTP/2和HTTP/3。直连HTTPS且选择`http3`时，库使用quiche+BoringSSL逐项应用并严格校验捕获的ClientHello、QUIC Transport Parameters原始wire与结构化字段、HTTP/3 SETTINGS顺序、Header顺序和QPACK策略；不支持完整回放的记录或场景会降级H2/H1.1，不会从通用或半指纹H3传输返回源`fingerprint_id`。

H3线级对照确认Chrome Initial使用8字节DCID、0字节SCID和1250字节UDP datagram；源与回放QUIC Transport Parameters语义和顺序、HTTP/3 SETTINGS顺序、Header名称和值均一致，QPACK编码逐字节一致。Session构造期会拒绝TLS payload摘要、QUIC raw wire、参数ID/长度/值、顶层语义字段、SETTINGS顺序或DATAGRAM联动不一致的schema 2记录。

新增浏览器profile仍必须经过相同门禁：正式产品身份、完整版本、真实TLS/HTTP2/H3捕获、未知字段严格校验、线级ClientHello和QUIC/H3/QPACK对照、Header顺序及HTTP/2参数回归。不能通过删除新扩展、改UA、复制旧记录或只复用Header伪造支持。
