use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use windows::core::{BOOL, BSTR};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, POINT, RECT, TRUE, WPARAM};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{
    OleGetClipboard, OleInitialize, OleSetClipboard, SafeArrayDestroy, SafeArrayGetElement,
    SafeArrayGetLBound, SafeArrayGetUBound, CF_UNICODETEXT,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationTreeWalker,
};
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows::Win32::UI::WindowsAndMessaging::{
    ChildWindowFromPoint, EnumWindows, GetClassNameW, GetWindow, GetWindowRect,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, PostMessageW, GW_OWNER,
    WA_ACTIVE, WM_ACTIVATE, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN,
    WM_MBUTTONUP, WM_MOUSEMOVE, WM_RBUTTONDOWN, WM_RBUTTONUP,
};

use crate::pipeline;
use crate::server::AppState;
use crate::urlutil::parse_article_url;

const SCAN_INTERVAL: Duration = Duration::from_secs(30);
const CLICK_GAP: Duration = Duration::from_secs(5);

pub fn start(state: Arc<AppState>) {
    let runtime = tokio::runtime::Handle::current();
    thread::Builder::new()
        .name("wechat-watch".into())
        .spawn(move || {
            if let Err(err) = watch(runtime, state) {
                tracing::warn!("微信窗口监听没有启动：{err}");
            }
        })
        .ok();
}

fn watch(runtime: tokio::runtime::Handle, state: Arc<AppState>) -> windows::core::Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let _ = OleInitialize(None);
        let automation: IUIAutomation =
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;
        let walker = automation.RawViewWalker()?;
        let mut tabs: HashMap<TabKey, TabMemo> = HashMap::new();
        let mut last_mouse: Option<Instant> = None;
        loop {
            let started = Instant::now();
            scan(
                &runtime,
                &state,
                &automation,
                &walker,
                &mut tabs,
                &mut last_mouse,
            );
            let elapsed = started.elapsed();
            if elapsed < SCAN_INTERVAL {
                thread::sleep(SCAN_INTERVAL - elapsed);
            }
        }
    }
}

fn scan(
    runtime: &tokio::runtime::Handle,
    state: &AppState,
    automation: &IUIAutomation,
    walker: &IUIAutomationTreeWalker,
    tabs: &mut HashMap<TabKey, TabMemo>,
    last_mouse: &mut Option<Instant>,
) {
    let windows = wechat_windows();
    let mut iconic = HashSet::new();
    let mut alive = HashSet::new();
    let mut seen = Vec::new();
    for hwnd in windows {
        let key = hwnd.0 as isize;
        alive.insert(key);
        if unsafe { IsIconic(hwnd).as_bool() } {
            iconic.insert(key);
            continue;
        }
        collect_tabs(walker, automation, hwnd, &mut seen);
    }
    let seen_keys: HashSet<TabKey> = seen.iter().map(|tab| tab.key.clone()).collect();
    tabs.retain(|key, _| {
        alive.contains(&key.hwnd) && (iconic.contains(&key.hwnd) || seen_keys.contains(key))
    });

    let mut saves = Vec::new();
    let mut clicks = Vec::new();
    let mut closes = Vec::new();
    let now = Instant::now();
    for tab in &seen {
        let memo = tabs.entry(tab.key.clone()).or_default();
        if memo.saved && memo.close_pending {
            closes.push(tab.clone());
        } else if let Some(url) = memo.url.clone() {
            if !memo.saved
                && !memo.skipped
                && click_is_due(memo.last_save, now, CLICK_GAP)
            {
                saves.push((tab.key.clone(), url));
            }
        } else if !memo.skipped
            && click_is_due(memo.last_right_click, now, CLICK_GAP)
            && click_is_due(*last_mouse, now, CLICK_GAP)
        {
            clicks.push(tab.clone());
        }
    }

    for (key, url) in saves {
        if let Some(memo) = tabs.get_mut(&key) {
            memo.last_save = Some(Instant::now());
            if save_article(runtime, state, &url) {
                memo.saved = true;
                memo.close_pending = state.close_wechat_tab.load(Ordering::Relaxed);
            }
        }
    }
    for tab in closes {
        wait_click_gap(last_mouse);
        post_mouse(tab.hwnd, tab.x, tab.y, MouseButton::Middle);
        *last_mouse = Some(Instant::now());
        if let Some(memo) = tabs.get_mut(&tab.key) {
            memo.close_pending = false;
        }
    }
    for tab in clicks {
        wait_click_gap(last_mouse);
        let copied = copy_article_link(automation, walker, tab.hwnd, tab.x, tab.y);
        *last_mouse = Some(Instant::now());
        let Some(memo) = tabs.get_mut(&tab.key) else {
            continue;
        };
        memo.last_right_click = Some(Instant::now());
        match copied {
            Copy::Article(url) => {
                memo.url = Some(url.clone());
                memo.last_save = Some(Instant::now());
                if save_article(runtime, state, &url) {
                    memo.saved = true;
                    if state.close_wechat_tab.load(Ordering::Relaxed)
                        && !unsafe { IsIconic(tab.hwnd).as_bool() }
                    {
                        post_mouse(tab.hwnd, tab.x, tab.y, MouseButton::Middle);
                        *last_mouse = Some(Instant::now());
                    } else if state.close_wechat_tab.load(Ordering::Relaxed) {
                        memo.close_pending = true;
                    }
                }
            }
            Copy::Other => memo.skipped = true,
            Copy::Miss => {}
        }
    }
}

fn save_article(runtime: &tokio::runtime::Handle, state: &AppState, url: &str) -> bool {
    match runtime.block_on(async {
        let _guard = state.lock.lock().await;
        pipeline::clip_article(&state.client, &state.output, url, None, false).await
    }) {
        Ok(saved) => {
            tracing::info!("{}：{}", saved.message, saved.title);
            true
        }
        Err(err) => {
            tracing::warn!("微信窗口剪藏失败：{err}");
            false
        }
    }
}

#[derive(Clone)]
struct SeenTab {
    key: TabKey,
    hwnd: HWND,
    x: i32,
    y: i32,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct TabKey {
    hwnd: isize,
    id: Vec<i32>,
}

#[derive(Default)]
struct TabMemo {
    url: Option<String>,
    skipped: bool,
    saved: bool,
    close_pending: bool,
    last_right_click: Option<Instant>,
    last_save: Option<Instant>,
}

fn collect_tabs(
    walker: &IUIAutomationTreeWalker,
    automation: &IUIAutomation,
    hwnd: HWND,
    found: &mut Vec<SeenTab>,
) {
    let Ok(root) = (unsafe { automation.ElementFromHandle(hwnd) }) else {
        return;
    };
    let mut window = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut window).is_err() } {
        return;
    }
    let mut seen = 0u32;
    walk_tabs(walker, &root, hwnd, window, found, &mut seen);
}

fn walk_tabs(
    walker: &IUIAutomationTreeWalker,
    element: &IUIAutomationElement,
    hwnd: HWND,
    window: RECT,
    found: &mut Vec<SeenTab>,
    seen: &mut u32,
) {
    if *seen > 250 {
        return;
    }
    *seen += 1;
    let class = current_class(element);
    if class == "Chrome_RenderWidgetHostHWND" || is_article_class(&class) {
        return;
    }
    if class == "Tab" {
        if let Some((x, y)) = tab_center(element, window) {
            if let Some(id) = runtime_id(element) {
                found.push(SeenTab {
                    key: TabKey {
                        hwnd: hwnd.0 as isize,
                        id,
                    },
                    hwnd,
                    x,
                    y,
                });
            }
        }
    }
    let mut child = unsafe { walker.GetFirstChildElement(element).ok() };
    let mut count = 0;
    while let Some(item) = child {
        if count > 40 {
            break;
        }
        walk_tabs(walker, &item, hwnd, window, found, seen);
        child = unsafe { walker.GetNextSiblingElement(&item).ok() };
        count += 1;
    }
}

fn tab_center(element: &IUIAutomationElement, window: RECT) -> Option<(i32, i32)> {
    let rect = current_rect(element)?;
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    let in_strip = rect.top >= window.top.saturating_sub(4)
        && rect.bottom <= window.top + 80
        && rect.left >= window.left.saturating_sub(4)
        && rect.right <= window.right.saturating_add(4);
    if in_strip && width >= 16 && height >= 16 && height <= 48 {
        Some(((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2))
    } else {
        None
    }
}

fn runtime_id(element: &IUIAutomationElement) -> Option<Vec<i32>> {
    unsafe {
        let array = element.GetRuntimeId().ok()?;
        if array.is_null() {
            return None;
        }
        let lower = SafeArrayGetLBound(array, 1).unwrap_or(0);
        let upper = SafeArrayGetUBound(array, 1).unwrap_or(-1);
        let mut ids = Vec::new();
        for index in lower..=upper {
            let mut value = 0i32;
            if SafeArrayGetElement(array, &index, &mut value as *mut i32 as *mut _).is_ok() {
                ids.push(value);
            }
        }
        let _ = SafeArrayDestroy(array);
        if ids.is_empty() {
            None
        } else {
            Some(ids)
        }
    }
}

enum Copy {
    Article(String),
    Other,
    Miss,
}

fn copy_article_link(
    automation: &IUIAutomation,
    walker: &IUIAutomationTreeWalker,
    hwnd: HWND,
    x: i32,
    y: i32,
) -> Copy {
    let clipboard = ClipboardGuard::capture();
    let mut opened = false;
    let result = (|| {
        post_mouse(hwnd, x, y, MouseButton::Right);
        opened = true;
        let item = wait_for_copy_item(automation, walker, hwnd)?;
        let menu = native_hwnd(&item).unwrap_or(hwnd);
        let (ix, iy) = current_rect(&item).map(rect_center).unwrap_or((x, y));
        post_mouse(menu, ix, iy, MouseButton::Left);
        thread::sleep(Duration::from_millis(80));
        let url = clipboard_text()?;
        if is_wechat_article_link(&url) {
            Some(Copy::Article(url))
        } else {
            Some(Copy::Other)
        }
    })();
    if opened {
        post_escape(hwnd);
    }
    clipboard.restore();
    result.unwrap_or(Copy::Miss)
}

fn native_hwnd(element: &IUIAutomationElement) -> Option<HWND> {
    let hwnd = unsafe { element.CurrentNativeWindowHandle().ok() }?;
    if hwnd.is_invalid() {
        None
    } else {
        Some(hwnd)
    }
}

fn rect_center(rect: RECT) -> (i32, i32) {
    ((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2)
}

enum MouseButton {
    Left,
    Right,
    Middle,
}

fn post_mouse(root: HWND, x: i32, y: i32, button: MouseButton) {
    unsafe {
        let screen = POINT { x, y };
        let mut client = screen;
        if !ScreenToClient(root, &mut client).as_bool() {
            return;
        }
        let mut target = ChildWindowFromPoint(root, client);
        if target.is_invalid() {
            target = root;
        }
        let mut at = screen;
        if !ScreenToClient(target, &mut at).as_bool() {
            return;
        }
        let _ = PostMessageW(
            Some(target),
            WM_ACTIVATE,
            WPARAM(WA_ACTIVE as usize),
            LPARAM(0),
        );
        let lparam = LPARAM(point_lparam(at.x, at.y));
        let (down, up, mk) = match button {
            MouseButton::Left => (WM_LBUTTONDOWN, WM_LBUTTONUP, 1usize),
            MouseButton::Right => (WM_RBUTTONDOWN, WM_RBUTTONUP, 2usize),
            MouseButton::Middle => (WM_MBUTTONDOWN, WM_MBUTTONUP, 16usize),
        };
        let _ = PostMessageW(Some(target), WM_MOUSEMOVE, WPARAM(0), lparam);
        thread::sleep(Duration::from_millis(30));
        let _ = PostMessageW(Some(target), down, WPARAM(mk), lparam);
        thread::sleep(Duration::from_millis(30));
        let _ = PostMessageW(Some(target), up, WPARAM(0), lparam);
    }
}

fn post_escape(hwnd: HWND) {
    unsafe {
        let key = WPARAM(VK_ESCAPE.0 as usize);
        let _ = PostMessageW(Some(hwnd), WM_KEYDOWN, key, LPARAM(0));
        thread::sleep(Duration::from_millis(20));
        let _ = PostMessageW(Some(hwnd), WM_KEYUP, key, LPARAM(0));
    }
}

fn wait_click_gap(last: &mut Option<Instant>) {
    if let Some(last_at) = *last {
        let elapsed = last_at.elapsed();
        if elapsed < CLICK_GAP {
            thread::sleep(CLICK_GAP - elapsed);
        }
    }
}

struct WindowList<'a> {
    pids: &'a HashSet<u32>,
    found: &'a mut Vec<HWND>,
}

fn wechat_windows() -> Vec<HWND> {
    let mut found = Vec::new();
    let pids = wechat_pids();
    unsafe {
        let mut payload = WindowList {
            pids: &pids,
            found: &mut found,
        };
        let _ = EnumWindows(
            Some(collect_window),
            LPARAM(&mut payload as *mut WindowList as isize),
        );
    }
    found
}

unsafe extern "system" fn collect_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let payload = &mut *(lparam.0 as *mut WindowList);
    if !IsWindowVisible(hwnd).as_bool() {
        return TRUE;
    }
    let mut class = [0u16; 64];
    let length = GetClassNameW(hwnd, &mut class);
    let class = String::from_utf16_lossy(&class[..length as usize]);
    if class != "Chrome_WidgetWin_0" {
        return TRUE;
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if payload.pids.contains(&pid) {
        payload.found.push(hwnd);
    }
    TRUE
}

fn wechat_pids() -> HashSet<u32> {
    let mut pids = HashSet::new();
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return pids;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..std::mem::zeroed()
        };
        if Process32FirstW(snapshot, &mut entry).is_err() {
            let _ = windows::Win32::Foundation::CloseHandle(snapshot);
            return pids;
        }
        loop {
            let name = wide_prefix(&entry.szExeFile);
            if name.eq_ignore_ascii_case("WeChatAppEx.exe") {
                pids.insert(entry.th32ProcessID);
            }
            if Process32NextW(snapshot, &mut entry).is_err() {
                break;
            }
        }
        let _ = windows::Win32::Foundation::CloseHandle(snapshot);
    }
    pids
}

struct NameSearch<'a> {
    automation: &'a IUIAutomation,
    walker: &'a IUIAutomationTreeWalker,
    wanted: &'a str,
    pids: &'a HashSet<u32>,
    target: HWND,
    found: &'a mut Option<IUIAutomationElement>,
}

fn wait_for_copy_item(
    automation: &IUIAutomation,
    walker: &IUIAutomationTreeWalker,
    hwnd: HWND,
) -> Option<IUIAutomationElement> {
    for _ in 0..8 {
        if let Some(item) = find_named(automation, walker, hwnd, "复制链接") {
            return Some(item);
        }
        thread::sleep(Duration::from_millis(40));
    }
    None
}

fn find_named(
    automation: &IUIAutomation,
    walker: &IUIAutomationTreeWalker,
    target: HWND,
    wanted: &str,
) -> Option<IUIAutomationElement> {
    let mut found = None;
    let pids = wechat_pids();
    unsafe {
        let mut payload = NameSearch {
            automation,
            walker,
            wanted,
            pids: &pids,
            target,
            found: &mut found,
        };
        let _ = EnumWindows(
            Some(find_in_window),
            LPARAM(&mut payload as *mut NameSearch as isize),
        );
    }
    found
}

unsafe extern "system" fn find_in_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let payload = &mut *(lparam.0 as *mut NameSearch);
    if payload.found.is_some() {
        return TRUE;
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if !payload.pids.contains(&pid) {
        return TRUE;
    }
    let owner = GetWindow(hwnd, GW_OWNER).unwrap_or_default();
    if hwnd != payload.target && owner != payload.target {
        return TRUE;
    }
    let Ok(root) = payload.automation.ElementFromHandle(hwnd) else {
        return TRUE;
    };
    if let Some(item) = find_named_in(payload.walker, &root, payload.wanted, &mut 0) {
        *payload.found = Some(item);
    }
    TRUE
}

fn find_named_in(
    walker: &IUIAutomationTreeWalker,
    element: &IUIAutomationElement,
    wanted: &str,
    seen: &mut u32,
) -> Option<IUIAutomationElement> {
    if *seen > 200 {
        return None;
    }
    *seen += 1;
    if current_name(element) == wanted {
        return Some(element.clone());
    }
    let mut child = unsafe { walker.GetFirstChildElement(element).ok() };
    let mut count = 0;
    while let Some(item) = child {
        if count > 40 {
            break;
        }
        if let Some(found) = find_named_in(walker, &item, wanted, seen) {
            return Some(found);
        }
        child = unsafe { walker.GetNextSiblingElement(&item).ok() };
        count += 1;
    }
    None
}

fn current_name(element: &IUIAutomationElement) -> String {
    unsafe { element.CurrentName() }
        .ok()
        .map(|value| bstr_string(&value))
        .unwrap_or_default()
}

fn current_class(element: &IUIAutomationElement) -> String {
    unsafe { element.CurrentClassName() }
        .ok()
        .map(|value| bstr_string(&value))
        .unwrap_or_default()
}

fn current_rect(element: &IUIAutomationElement) -> Option<RECT> {
    unsafe { element.CurrentBoundingRectangle().ok() }
}

fn bstr_string(value: &BSTR) -> String {
    String::from_utf16_lossy(value)
}

pub(crate) fn is_article_class(class_name: &str) -> bool {
    class_name
        .split_whitespace()
        .any(|part| part == "rich_media" || part.starts_with("rich_media_"))
}

fn is_wechat_article_link(raw: &str) -> bool {
    parse_article_url(raw.trim())
        .ok()
        .is_some_and(|url| url.host_str() == Some("mp.weixin.qq.com"))
}

pub(crate) fn click_is_due(last: Option<Instant>, now: Instant, gap: Duration) -> bool {
    match last {
        Some(last) => now.saturating_duration_since(last) >= gap,
        None => true,
    }
}

pub(crate) fn point_lparam(x: i32, y: i32) -> isize {
    let packed = ((y as u16 as u32) << 16) | (x as u16 as u32);
    packed as i32 as isize
}

struct ClipboardGuard {
    saved: Option<windows::Win32::System::Com::IDataObject>,
}

impl ClipboardGuard {
    fn capture() -> Self {
        let saved = unsafe { OleGetClipboard().ok() };
        Self { saved }
    }

    fn restore(self) {
        unsafe {
            if let Some(saved) = self.saved.as_ref() {
                let _ = OleSetClipboard(saved);
            } else if OpenClipboard(None).is_ok() {
                let _ = EmptyClipboard();
                let _ = CloseClipboard();
            }
        }
    }
}

fn clipboard_text() -> Option<String> {
    for _ in 0..5 {
        if let Some(text) = read_clipboard_text() {
            let text = text.trim().to_string();
            if !text.is_empty() {
                return Some(text);
            }
        }
        thread::sleep(Duration::from_millis(30));
    }
    None
}

fn read_clipboard_text() -> Option<String> {
    unsafe {
        OpenClipboard(None).ok()?;
        let handle = GetClipboardData(CF_UNICODETEXT.0 as u32).ok();
        let text = handle.and_then(|handle| global_text(handle));
        let _ = CloseClipboard();
        text
    }
}

fn global_text(handle: HANDLE) -> Option<String> {
    unsafe {
        let ptr = GlobalLock(windows::Win32::Foundation::HGLOBAL(handle.0));
        if ptr.is_null() {
            return None;
        }
        let mut len = 0usize;
        while *ptr.cast::<u16>().add(len) != 0 {
            len += 1;
            if len > 4096 {
                break;
            }
        }
        let slice = std::slice::from_raw_parts(ptr.cast::<u16>(), len);
        let text = String::from_utf16_lossy(slice);
        let _ = GlobalUnlock(windows::Win32::Foundation::HGLOBAL(handle.0));
        Some(text)
    }
}

fn wide_prefix(value: &[u16]) -> String {
    let end = value.iter().position(|ch| *ch == 0).unwrap_or(value.len());
    String::from_utf16_lossy(&value[..end])
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{click_is_due, is_article_class, point_lparam};

    #[test]
    fn article_page_classes_match() {
        assert!(is_article_class("rich_media"));
        assert!(is_article_class("rich_media_inner"));
        assert!(is_article_class(
            "zh_CN wx_wap_page rich_media_bottom_bar pages_skin_pc"
        ));
    }

    #[test]
    fn other_wechat_pages_do_not_match() {
        assert!(!is_article_class("Chrome_WidgetWin_0"));
        assert!(!is_article_class("OmniboxViewViews"));
        assert!(!is_article_class(""));
    }

    #[test]
    fn right_click_waits_five_seconds() {
        let now = Instant::now();
        assert!(click_is_due(None, now, Duration::from_secs(5)));
        assert!(!click_is_due(
            Some(now - Duration::from_secs(4)),
            now,
            Duration::from_secs(5)
        ));
        assert!(click_is_due(
            Some(now - Duration::from_secs(5)),
            now,
            Duration::from_secs(5)
        ));
    }

    #[test]
    fn client_point_packs_into_message_lparam() {
        assert_eq!(point_lparam(192, 22), ((22 << 16) | 192) as isize);
    }
}
