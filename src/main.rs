#![forbid(unsafe_code)]

mod article;
mod error;
mod fetch;
mod pipeline;
mod server;
mod store;
mod urlutil;

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use tracing_subscriber::EnvFilter;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Command {
    Serve,
    Clip,
}

struct Opts {
    command: Command,
    addr: String,
    output: PathBuf,
    force: bool,
    url: Option<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let opts = match parse_args() {
        Ok(opts) => opts,
        Err(message) => {
            eprintln!("{message}");
            eprint_help();
            return ExitCode::from(2);
        }
    };

    match opts.command {
        Command::Serve => {
            let origin = public_origin(&opts.addr);
            println!("剪藏服务已启动：{origin}");
            println!("文章会保存到：{}", opts.output.display());
            println!("油猴脚本：{origin}/wx-clipper.user.js");
            if let Err(err) = server::serve(&opts.addr, opts.output, origin).await {
                eprintln!("服务启动失败：{err}");
                return ExitCode::from(1);
            }
            ExitCode::SUCCESS
        }
        Command::Clip => {
            let url = opts.url.expect("url");
            let client = fetch::build_client();
            match pipeline::clip_article(&client, &opts.output, &url, None, opts.force).await {
                Ok(output) => {
                    println!("{}：{}", output.message, output.title);
                    println!("Markdown：{}", output.markdown_path);
                    println!("预览：{}", output.preview_path);
                    if output.image_failed > 0 {
                        println!(
                            "图片：{} 张已保存，{} 张失败",
                            output.image_count, output.image_failed
                        );
                    } else {
                        println!("图片：{} 张", output.image_count);
                    }
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("剪藏失败：{err}");
                    if err.need_html() {
                        eprintln!(
                            "浏览器里如果能打开这篇文章，让油猴脚本把当前页面回传给本地服务。"
                        );
                    }
                    ExitCode::from(1)
                }
            }
        }
    }
}

fn parse_args() -> Result<Opts, String> {
    let mut command = Command::Serve;
    let mut command_set = false;
    let mut addr = env::var("WXCLIP_ADDR").unwrap_or_else(|_| "127.0.0.1:17331".into());
    let mut output = PathBuf::from(env::var("WXCLIP_OUT").unwrap_or_else(|_| "clips".into()));
    let mut force = false;
    let mut positionals = Vec::new();
    let args: Vec<String> = env::args().skip(1).collect();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        match arg.as_str() {
            "serve" if !command_set && positionals.is_empty() => {
                command = Command::Serve;
                command_set = true;
            }
            "clip" if !command_set && positionals.is_empty() => {
                command = Command::Clip;
                command_set = true;
            }
            "--addr" => {
                index += 1;
                addr = args
                    .get(index)
                    .cloned()
                    .ok_or_else(|| "缺少 --addr 的值".to_string())?;
            }
            "--output" | "-o" => {
                index += 1;
                output = PathBuf::from(
                    args.get(index)
                        .cloned()
                        .ok_or_else(|| "缺少 --output 的值".to_string())?,
                );
            }
            "--force" | "-f" => force = true,
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            other if other.starts_with('-') => return Err(format!("未知参数 {other}")),
            other => positionals.push(other.to_string()),
        }
        index += 1;
    }
    let mut url = None;
    if command == Command::Serve && positionals.len() == 1 && positionals[0].contains("://") {
        command = Command::Clip;
        url = positionals.pop();
    }
    if command == Command::Clip {
        url = url.or_else(|| positionals.first().cloned());
        if url.is_none() {
            return Err("clip 需要一篇公众号文章链接".into());
        }
    } else if !positionals.is_empty() {
        return Err(format!("多余参数 {}", positionals[0]));
    }
    Ok(Opts {
        command,
        addr,
        output,
        force,
        url,
    })
}

fn public_origin(addr: &str) -> String {
    let (host, port) = split_host_port(addr);
    let host = if host == "0.0.0.0" || host == "::" || host == "[::]" {
        "127.0.0.1"
    } else {
        host
    };
    format!("http://{host}:{port}")
}

fn split_host_port(addr: &str) -> (&str, &str) {
    if let Some(rest) = addr.strip_prefix('[') {
        if let Some((host, port)) = rest.split_once("]:") {
            return (host, port);
        }
    }
    match addr.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && port.chars().all(|ch| ch.is_ascii_digit()) => {
            (host, port)
        }
        _ => (addr, "17331"),
    }
}

fn print_help() {
    println!(
        r#"微信公众号剪藏

用法
  wx-clipper serve [--addr 127.0.0.1:17331] [--output clips]
  wx-clipper clip <文章链接> [--output clips] [--force]
  wx-clipper <文章链接>

打开 http://127.0.0.1:17331 查看已保存的文章，并安装油猴脚本。
浏览器打开公众号文章时，脚本会把链接发给本机服务。

环境变量
  WXCLIP_ADDR   监听地址，默认 127.0.0.1:17331
  WXCLIP_OUT    保存目录，默认 clips"#
    );
}

fn eprint_help() {
    eprintln!("运行 wx-clipper --help 查看用法");
}
