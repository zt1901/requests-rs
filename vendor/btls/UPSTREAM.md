# btls本地覆盖说明

- 上游仓库：`https://github.com/0x676e67/btls.git`
- btls提交：`ab7f522`
- BoringSSL提交：`91a66a59b6c1435120ff83e245d7719411294386`
- Cargo覆盖：`btls-sys = { path = "vendor/btls/btls-sys" }`

本地修改仅用于补齐Chrome 150已经启用、但该BoringSSL提交尚未注册到TLS `libssl`的ML-DSA SignatureScheme：

- `0x0904`：ML-DSA-44，配置名 `mldsa44`
- `0x0905`：ML-DSA-65，配置名 `mldsa65`
- `0x0906`：ML-DSA-87，配置名 `mldsa87`

修改位置：

- `btls-sys/deps/boringssl/include/openssl/ssl.h`
- `btls-sys/deps/boringssl/ssl/ssl_privkey.cc`

加密实现原本已经存在，本地只按BoringSSL新上游实现注册TLS codepoint、算法能力和配置名称。Chrome 150回归已验证JA4、Signature Algorithms、ClientHello长度模式和扩展线级摘要，最终为41/41。

Edge 152补充：`edge152`只保留一条本机Edge 152.0.4191.53火种记录。同一wreq Client复用匹配路由的连接；连接池自然新建TLS时扩展顺序重新随机，JA3可变而JA3N、JA4和火种ID稳定。该能力不需要新增BoringSSL补丁。

Firefox 151补充：

- Profile关闭普通GREASE时，ECH GREASE使用Firefox固定最大桶和NSS载荷长度。
- Profile开启普通GREASE时，继续使用Chrome的随机32字节桶，不改变Chrome ECH随机化。
- build script显式监听三个本地BoringSSL补丁文件，避免增量构建误用旧静态库。

Firefox 151五次独立完整握手的JA3、JA4、ClientHello长度、扩展顺序和HTTP/2参数全部一致，因此`firefox151`只保留一条火种并固定NSS扩展顺序。TLS票据恢复时由共享Session Cache自然增加PSK扩展41，不保存恢复握手为第二条火种。

最终同一wheel下Chrome 150通过41/41；所有受支持浏览器版本均收敛为单火种。Chrome/Edge只在新TLS连接上产生随机扩展顺序，Firefox保留固定完整握手JA3和协议自然产生的PSK恢复JA3。
