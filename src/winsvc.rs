use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, OpenProcessToken, WaitForSingleObject, INFINITE,
};
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows_service::service::{
    ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
    ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

const SERVICE_NAME: &str = "wx-clipper";
const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
const ERROR_SERVICE_EXISTS: i32 = 1073;

pub fn install(addr: &str, output: &Path, relay: Option<&Path>) -> ExitCode {
    if !is_elevated() {
        return elevate("install", addr, output);
    }
    match register_and_start(addr, output) {
        Ok(()) => {
            say(
                relay,
                false,
                &format!(
                    "服务已安装并启动。开机后会自动运行。\n文章会保存到：{}",
                    output.display()
                ),
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            say(relay, true, &format!("安装服务失败：{err}"));
            ExitCode::from(1)
        }
    }
}

pub fn uninstall(relay: Option<&Path>) -> ExitCode {
    if !is_elevated() {
        return elevate("uninstall", "", Path::new(""));
    }
    match remove_service() {
        Ok(RemoveResult::Removed) => {
            say(relay, false, "服务已卸载。已保存的文章还在。");
            ExitCode::SUCCESS
        }
        Ok(RemoveResult::Missing) => {
            say(relay, false, "没有已安装的服务。");
            ExitCode::SUCCESS
        }
        Err(err) => {
            say(relay, true, &format!("卸载服务失败：{err}"));
            ExitCode::from(1)
        }
    }
}

pub fn run() -> ExitCode {
    if let Err(err) = service_dispatcher::start(SERVICE_NAME, ffi_service_main) {
        eprintln!("这个命令由 Windows 服务管理器启动：{err}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

define_windows_service!(ffi_service_main, service_main);

fn service_main(_arguments: Vec<OsString>) {
    let _ = run_service();
}

fn run_service() -> Result<(), String> {
    let (addr, output) = service_runtime_opts();
    let origin = super::public_origin(&addr);
    let notify = Arc::new(Notify::new());
    let notify_for_handler = notify.clone();
    let status_handle =
        service_control_handler::register(SERVICE_NAME, move |control| match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                notify_for_handler.notify_one();
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        })
        .map_err(|err| err.to_string())?;
    status_handle
        .set_service_status(status(ServiceState::Running, stop_controls()))
        .map_err(|err| err.to_string())?;
    let handle_for_stop = status_handle.clone();
    let runtime = tokio::runtime::Runtime::new().map_err(|err| err.to_string())?;
    let result = runtime.block_on(super::server::serve(&addr, output, origin, async move {
        notify.notified().await;
        let _ = handle_for_stop.set_service_status(status(
            ServiceState::StopPending,
            ServiceControlAccept::empty(),
        ));
    }));
    let _ = status_handle
        .set_service_status(status(ServiceState::Stopped, ServiceControlAccept::empty()));
    result.map_err(|err| err.to_string())
}

fn elevate(command: &str, addr: &str, output: &Path) -> ExitCode {
    let exe = match env::current_exe() {
        Ok(path) => path,
        Err(err) => {
            eprintln!("找不到程序自身：{err}");
            return ExitCode::from(1);
        }
    };
    let relay = env::temp_dir().join(format!("wx-clipper-auth-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&relay);
    let relay_arg = quote_arg(&relay.display().to_string());
    let parameters = match command {
        "install" => format!(
            "install --addr {} --output {} --relay {}",
            quote_arg(addr),
            quote_arg(&output.display().to_string()),
            relay_arg
        ),
        _ => format!("uninstall --relay {relay_arg}"),
    };
    let outcome = relaunch(&exe, &parameters);
    let text = std::fs::read_to_string(&relay).unwrap_or_default();
    let _ = std::fs::remove_file(&relay);
    match outcome {
        Relaunch::Cancelled => {
            eprintln!("已取消");
            ExitCode::from(1)
        }
        Relaunch::Finished(0) => {
            let fallback = if command == "install" {
                "服务已安装并启动。开机后会自动运行。"
            } else {
                "服务已卸载。已保存的文章还在。"
            };
            relay_text(false, &text, fallback);
            ExitCode::SUCCESS
        }
        Relaunch::Finished(_) => {
            let fallback = if command == "install" {
                "安装服务失败"
            } else {
                "卸载服务失败"
            };
            relay_text(true, &text, fallback);
            ExitCode::from(1)
        }
        Relaunch::Failed(err) => {
            eprintln!("无法弹出授权框：{err}");
            ExitCode::from(1)
        }
    }
}

fn say(relay: Option<&Path>, error: bool, message: &str) {
    if error {
        eprintln!("{message}");
    } else {
        println!("{message}");
    }
    if let Some(path) = relay {
        let _ = std::fs::write(path, message);
    }
}

fn relay_text(error: bool, text: &str, fallback: &str) {
    let message = if text.trim().is_empty() {
        fallback
    } else {
        text
    };
    if error {
        eprintln!("{message}");
    } else {
        println!("{message}");
    }
}

enum Relaunch {
    Cancelled,
    Finished(u32),
    Failed(String),
}

fn relaunch(exe: &Path, parameters: &str) -> Relaunch {
    let file = wide(exe);
    let params = wide(parameters);
    let verb = wide("runas");
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    if let Err(err) = unsafe { ShellExecuteExW(&mut info) } {
        if is_cancelled(&err) {
            return Relaunch::Cancelled;
        }
        return Relaunch::Failed(err.to_string());
    }
    let process = info.hProcess;
    if process.is_invalid() {
        return Relaunch::Failed("授权后的进程没有返回".into());
    }
    unsafe {
        WaitForSingleObject(process, INFINITE);
        let mut code = 0u32;
        let read = GetExitCodeProcess(process, &mut code);
        let _ = CloseHandle(process);
        if read.is_err() {
            return Relaunch::Failed("读不到授权后进程的结果".into());
        }
        Relaunch::Finished(code)
    }
}

fn is_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut returned = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut TOKEN_ELEVATION as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        );
        let _ = CloseHandle(token);
        ok.is_ok() && elevation.TokenIsElevated != 0
    }
}

fn register_and_start(addr: &str, output: &Path) -> Result<(), String> {
    let info = service_info(addr, output)?;
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .map_err(|err| err.to_string())?;
    let (service, created) = match manager.create_service(
        &info,
        ServiceAccess::CHANGE_CONFIG
            | ServiceAccess::START
            | ServiceAccess::STOP
            | ServiceAccess::QUERY_STATUS
            | ServiceAccess::DELETE,
    ) {
        Ok(service) => (service, true),
        Err(err) if os_code(&err) == Some(ERROR_SERVICE_EXISTS) => {
            let service = manager
                .open_service(
                    SERVICE_NAME,
                    ServiceAccess::CHANGE_CONFIG
                        | ServiceAccess::START
                        | ServiceAccess::STOP
                        | ServiceAccess::QUERY_STATUS,
                )
                .map_err(|err| err.to_string())?;
            service
                .change_config(&info)
                .map_err(|err| err.to_string())?;
            stop_if_running(&service)?;
            (service, false)
        }
        Err(err) => return Err(err.to_string()),
    };
    if let Err(err) = service.start::<OsString>(&[]) {
        if created {
            let _ = service.delete();
        }
        return Err(err.to_string());
    }
    Ok(())
}

enum RemoveResult {
    Removed,
    Missing,
}

fn remove_service() -> Result<RemoveResult, String> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|err| err.to_string())?;
    let service = match manager.open_service(
        SERVICE_NAME,
        ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
    ) {
        Ok(service) => service,
        Err(err) if os_code(&err) == Some(ERROR_SERVICE_DOES_NOT_EXIST) => {
            return Ok(RemoveResult::Missing);
        }
        Err(err) => return Err(err.to_string()),
    };
    stop_if_running(&service)?;
    service.delete().map_err(|err| err.to_string())?;
    Ok(RemoveResult::Removed)
}

fn stop_if_running(service: &windows_service::service::Service) -> Result<(), String> {
    let current = service.query_status().map_err(|err| err.to_string())?;
    if current.current_state == ServiceState::Stopped {
        return Ok(());
    }
    service.stop().map_err(|err| err.to_string())?;
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(200));
        let current = service.query_status().map_err(|err| err.to_string())?;
        if current.current_state == ServiceState::Stopped {
            return Ok(());
        }
    }
    Err("服务没有在预期时间内停止".into())
}

fn service_info(addr: &str, output: &Path) -> Result<ServiceInfo, String> {
    Ok(ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from("剪藏"),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: env::current_exe().map_err(|err| err.to_string())?,
        launch_arguments: vec![
            OsString::from("service"),
            OsString::from("--addr"),
            OsString::from(addr),
            OsString::from("--output"),
            OsString::from(output.as_os_str()),
        ],
        dependencies: vec![],
        account_name: None,
        account_password: None,
    })
}

fn service_runtime_opts() -> (String, PathBuf) {
    let mut addr = "127.0.0.1:17331".to_string();
    let mut output = PathBuf::from("clips");
    let args: Vec<String> = env::args().skip(1).collect();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--addr" => {
                if let Some(value) = args.get(index + 1) {
                    addr = value.clone();
                }
                index += 1;
            }
            "--output" => {
                if let Some(value) = args.get(index + 1) {
                    output = PathBuf::from(value);
                }
                index += 1;
            }
            _ => {}
        }
        index += 1;
    }
    (addr, output)
}

fn status(state: ServiceState, controls: ServiceControlAccept) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: controls,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::from_secs(5),
        process_id: None,
    }
}

fn stop_controls() -> ServiceControlAccept {
    ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
}

fn os_code(err: &windows_service::Error) -> Option<i32> {
    match err {
        windows_service::Error::Winapi(io) => io.raw_os_error(),
        _ => None,
    }
}

fn is_cancelled(err: &windows::core::Error) -> bool {
    let code = err.code().0 as u32;
    code == ERROR_CANCELLED.0 || code & 0xFFFF == ERROR_CANCELLED.0
}

fn wide(value: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    value
        .as_ref()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn quote_arg(value: &str) -> String {
    if value.contains([' ', '\t', '"']) {
        format!("\"{}\"", value.replace('"', "\\\""))
    } else {
        value.to_string()
    }
}
