# AGENTS.md

## 油猴脚本兼容

`userscript/wx-clipper.user.js` 的 `@downloadURL` / `@updateURL` 指向默认分支。使用者电脑上的 `wx-clipper` 可能是更早的 Release。新脚本必须还能驱动这些旧服务。

当前契约是 `POST /api/clip`，请求 JSON：

- `url`：字符串，必填
- `html`：字符串，可省略
- `force`：布尔，可省略，默认 false

脚本完成一次剪藏只依赖这些响应字段：`ok`、`message`、`title`、`need_html`、`preview_url`。`need_html` 为真且本次没带 `html` 时，脚本会把当前文档再发一次。成功时只有 `preview_url` 以 `/files/` 开头才打开预览。

改脚本时：

- 继续发送上述请求。不要改成旧服务不认识的路径、方法或必填字段。
- 新响应字段只能当可选能力。字段缺失时仍按上面的成功、失败和 `need_html` 流程走完。
- 不要删除对 `need_html` 的回退，旧服务在被微信拦住时靠它要页面 HTML。

改服务时：

- 继续接受上述请求。未知字段忽略，不要把 `html` 或 `force` 改成必填。
- 不要移除或改名脚本依赖的响应字段，不要改变 `need_html` 和 `/files/` 预览地址的含义。
- 新能力用新的可选字段。必须换协议时另开路径，并让脚本在旧路径失败后仍能剪藏。
