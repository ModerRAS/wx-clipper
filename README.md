# 公众号剪藏

在浏览器打开一篇微信公众号文章时，把链接交给本机的 Rust 服务。服务抓取正文，把图片、音频和视频下载到本地，保存成 Markdown。

适合自己读过、想留档的文章。它不会按公众号批量拉历史，也不会登录微信。

## 它怎么工作

公开文章的正文在首次返回的 HTML 里，不需要先滚动页面。

| 内容 | 位置 |
| --- | --- |
| 标题 | `#activity-name`，其次 `og:title` |
| 公众号 | `#js_name` |
| 作者 | `#js_author_name` |
| 发布时间 | 脚本里的 `create_time`（Unix 秒），页面上的时间节点要等 JavaScript 才填上 |
| 正文 | `#js_content` |
| 图片、音频、视频 | 正文里的 `img`、`audio`、`video`、`source` 和背景图，下载到 `images/`、`audio/`、`video/` |

直接请求有时会遇到「环境异常」。这时油猴脚本会把浏览器里已经打开的页面 HTML 再发一次，服务改用这份正文，不再自己去抓。

```text
浏览器打开文章
    │  POST /api/clip  { url }
    ▼
本机 wx-clipper
    │  GET 文章 HTML
    │  GET 正文里的图片、音频和视频
    ▼
clips/<日期>-<标题>-<id>/
    article.md
    preview.html
    images/img-001.jpg
    audio/audio-001.mp3
    video/video-001.mp4
```

## 运行

没有 Rust 时，从 [Releases](https://github.com/ModerRAS/wx-clipper/releases) 下载对应系统的压缩包。Windows 解压后是 `wx-clipper.exe`，macOS 和 Linux 解压后是 `wx-clipper`。同页的 `SHA256SUMS` 用来核对文件。

需要 Rust 1.88 或更新版本。目录里的 `rust-toolchain.toml` 会让 rustup 选用当前 stable。

```bash
cargo run --release -- serve
```

然后打开 [http://127.0.0.1:17331](http://127.0.0.1:17331)。

也可以不启动服务，直接剪一条链接：

```bash
cargo run --release -- clip "https://mp.weixin.qq.com/s/98yW8HA6lcp0i5haDJWMbg"
```

文章默认写到当前目录的 `clips/`。换目录或端口：

```bash
cargo run --release -- serve --addr 127.0.0.1:17331 --output ~/notes/wechat
```

环境变量 `WXCLIP_ADDR`、`WXCLIP_OUT` 是同样的默认值。`--force` 会覆盖已经保存过的同一篇。

## 油猴脚本

1. 浏览器安装 [Tampermonkey](https://www.tampermonkey.net/) 或 Violentmonkey。
2. 打开 [wx-clipper.user.js](https://raw.githubusercontent.com/ModerRAS/wx-clipper/main/userscript/wx-clipper.user.js) 按提示安装。脚本之后从这条地址检查更新。源文件在 `userscript/wx-clipper.user.js`。服务已经启动时，也可以打开 [http://127.0.0.1:17331/wx-clipper.user.js](http://127.0.0.1:17331/wx-clipper.user.js)。
3. 打开 `https://mp.weixin.qq.com/s/...` 文章。右下角出现「公众号剪藏」，成功后可以打开本地预览。

脚本只匹配公众号文章页，并用 `GM_xmlhttpRequest` 访问本机，避免页面自身的跨域限制。服务只接受 `mp.weixin.qq.com` 的文章链接。正文里的图片、音频和视频会下载到本地，普通网页链接仍保留为链接。

如果服务端抓取被验证页拦住，脚本会自动把当前文档发回去再试一次。

## 输出

`article.md` 带 YAML 头（标题、公众号、作者、北京时间、原文链接）。正文媒体写成相对路径，例如 `images/img-001.jpg`、`audio/audio-001.mp3`、`video/video-001.mp4`。下载失败时，对应位置写成「图片缺失」「音频缺失」或「视频缺失」。`preview.html` 用同一份内容做本地预览。`clips/index.json` 用来按链接去重。

## 测试

```bash
cargo test
```

单元测试使用合成的公众号 HTML，不访问网络。
