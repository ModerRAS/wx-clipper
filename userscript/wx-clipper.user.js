// ==UserScript==
// @name         微信公众号剪藏
// @namespace    wx-clipper
// @version      0.1.0
// @description  打开公众号文章时，把链接发给本地剪藏服务，保存为带图片的 Markdown
// @match        https://mp.weixin.qq.com/s/*
// @match        https://mp.weixin.qq.com/s?*
// @run-at       document-idle
// @grant        GM_xmlhttpRequest
// @connect      127.0.0.1
// @connect      localhost
// @updateURL    https://raw.githubusercontent.com/ModerRAS/wx-clipper/main/userscript/wx-clipper.user.js
// @downloadURL  https://raw.githubusercontent.com/ModerRAS/wx-clipper/main/userscript/wx-clipper.user.js
// ==/UserScript==

(function () {
  "use strict";
  if (window.top !== window.self || window.__wxClipper) return;
  window.__wxClipper = true;

  const ORIGIN = "http://127.0.0.1:17331";
  const ENDPOINT = ORIGIN + "/api/clip";

  const page = new URL(location.href);
  const isArticle = page.hostname === "mp.weixin.qq.com" && (page.pathname === "/s" || page.pathname.startsWith("/s/"));
  if (!isArticle) return;

  const host = document.createElement("div");
  host.style.cssText = "all: initial; position: fixed; z-index: 2147483647;";
  document.documentElement.appendChild(host);
  const shadow = host.attachShadow({ mode: "open" });
  shadow.innerHTML = `
    <style>
      .panel { position: fixed; right: 16px; bottom: 16px; width: min(320px, calc(100vw - 32px));
        background: #fffdf9; color: #221f1b; border: 1px solid #e2dbd1; border-radius: 14px;
        box-shadow: 0 10px 30px rgba(40, 30, 10, 0.16); padding: 12px 14px; font: 14px/1.45 "Segoe UI", "PingFang SC", sans-serif; }
      strong { display: block; margin-bottom: 4px; }
      p { margin: 0; color: #6f675e; }
      .actions { display: flex; gap: 8px; margin-top: 8px; }
      button, a { font: inherit; border-radius: 999px; padding: 6px 10px; text-decoration: none; }
      button { border: 1px solid #e2dbd1; background: #fff; color: #221f1b; cursor: pointer; }
      a { background: #1d6b4f; color: #f4fbf7; }
    </style>
    <div class="panel">
      <strong>公众号剪藏</strong>
      <p id="status">准备把这篇发给本地服务…</p>
      <div class="actions">
        <button id="retry" type="button">重新剪藏</button>
        <a id="preview" hidden target="_blank" rel="noreferrer">打开预览</a>
      </div>
    </div>
  `;
  const statusEl = shadow.getElementById("status");
  const retry = shadow.getElementById("retry");
  const preview = shadow.getElementById("preview");

  function setStatus(text) {
    statusEl.textContent = text;
  }

  function post(payload) {
    setStatus(payload.html ? "服务器抓链接被拦住了，改用当前页面正文…" : "正在剪藏…");
    preview.hidden = true;
    GM_xmlhttpRequest({
      method: "POST",
      url: ENDPOINT,
      data: JSON.stringify(payload),
      headers: { "Content-Type": "application/json" },
      timeout: 120000,
      onload(response) {
        let data = {};
        try { data = JSON.parse(response.responseText || "{}"); } catch (error) { data = {}; }
        if (data.need_html && !payload.html) {
          if (!document.querySelector("#js_content")) {
            setStatus("服务器被微信拦住了，而当前页也没有正文。");
            return;
          }
          post({ url: payload.url, html: document.documentElement.outerHTML, force: payload.force });
          return;
        }
        if (response.status >= 200 && response.status < 300 && data.ok) {
          const title = data.title ? `「${data.title}」` : "这篇文章";
          setStatus(`${data.message || "已保存"}：${title}`);
          if (typeof data.preview_url === "string" && data.preview_url.startsWith("/files/")) {
            preview.href = ORIGIN + data.preview_url;
            preview.hidden = false;
          }
          return;
        }
        setStatus(data.message || `剪藏失败（HTTP ${response.status}）`);
      },
      onerror() {
        setStatus("连不上本地服务。先在电脑上运行 wx-clipper。");
      },
      ontimeout() {
        setStatus("本地服务超时了，可以点重新剪藏。");
      }
    });
  }

  retry.addEventListener("click", () => {
    post({ url: location.href.split("#")[0], force: true });
  });
  post({ url: location.href.split("#")[0], force: false });
})();
