# 上游锁定

- 仓库：`https://github.com/0x676e67/http2.git`
- 标签：`v0.5.20`
- 提交：`5a9a1fe28154461318310e6044959c6dc8a60d17`

本地修改：

- HPACK字符串仅在Huffman结果更短时压缩，对齐Chrome短值编码。
- 客户端首个SETTINGS ACK延迟到首个请求帧之后，对齐Chrome初始帧顺序。
- 首个Stream ID为3时启用Firefox/NSS HPACK模式：所有非空字面量使用Huffman，并对`:path`字面值选择静态表索引5。
- Firefox模式由Profile中的首个HEADERS Priority Stream ID驱动；Chrome首流1继续使用原策略。
