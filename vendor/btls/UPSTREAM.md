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

Firefox 151补充：

- Profile关闭普通GREASE时，ECH GREASE使用Firefox固定最大桶和NSS载荷长度。
- Profile开启普通GREASE时，继续使用Chrome的随机32字节桶，不改变Chrome ECH随机化。
- build script显式监听三个本地BoringSSL补丁文件，避免增量构建误用旧静态库。

最终同一wheel下Chrome 150和Firefox 151均通过41/41。
