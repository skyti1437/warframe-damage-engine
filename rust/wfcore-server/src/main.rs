use std::env;
use std::ffi::OsString;
use std::io::{self, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;
use wfcore_server::data::{locate_data_root, EngineData, Result};
use wfcore_server::http::Service;

const HELP: &str = "用法：wfcore-server [--data-dir <PATH>] [--host <IP>] [--port <PORT>] [--probe | --dump-catalog | --simulate-batch <JSON_PATH>]
  --data-dir      指定项目根目录或 data 目录，默认从工作目录和可执行文件位置查找
  --host          监听 IP，默认 127.0.0.1
  --port          监听端口，默认 8765
  --probe         加载数据并构建目录，输出健康 JSON 后退出
  --dump-catalog  输出与 GET /api/catalog 相同的 JSON 后退出
  --simulate-batch  读取 JSON 批次并逐条输出模拟状态与响应
  WF_SERVER_THREADS 指定 HTTP 工作线程数，默认 2";

#[derive(PartialEq, Eq)]
enum Mode { Serve, Probe, Dump, SimulateBatch(PathBuf) }

struct Options {
    directory: Option<PathBuf>,
    host: IpAddr,
    port: u16,
    mode: Mode,
}

impl Options {
    fn parse() -> Result<Option<Self>> {
        let mut options = Self { directory: None, host: Ipv4Addr::LOCALHOST.into(), port: 8765, mode: Mode::Serve };
        let mut args = env::args_os().skip(1);
        while let Some(option) = args.next() {
            let mut argument = || args.next().ok_or_else(|| format!("{} 缺少参数", option.to_string_lossy()));
            match option.to_str() {
                Some("--help" | "-h") => return Ok(None),
                Some("--data-dir") => options.directory = Some(PathBuf::from(argument()?)),
                Some("--host") => options.host = utf8(argument()?)?.parse().map_err(|_| "--host 必须为有效 IP")?,
                Some("--port") => options.port = utf8(argument()?)?.parse().map_err(|_| "--port 必须为 0–65535 的整数")?,
                Some(flag @ ("--probe" | "--dump-catalog" | "--simulate-batch")) => {
                    if options.mode != Mode::Serve {
                        return Err("--probe、--dump-catalog 和 --simulate-batch 只能指定一个".into());
                    }
                    options.mode = match flag {
                        "--probe" => Mode::Probe,
                        "--dump-catalog" => Mode::Dump,
                        _ => Mode::SimulateBatch(PathBuf::from(argument()?)),
                    };
                }
                _ => return Err(format!("未知参数 {}", option.to_string_lossy()).into()),
            }
        }
        Ok(Some(options))
    }
}

fn utf8(value: OsString) -> Result<String> {
    value.into_string().map_err(|_| "参数必须是 UTF-8 文本".into())
}

fn run() -> Result<()> {
    let Some(options) = Options::parse()? else {
        println!("{HELP}");
        return Ok(());
    };
    let root = match options.directory {
        Some(root) => root,
        None => locate_data_root(&env::current_dir()?, &env::current_exe()?)?,
    };
    let service = Service::new(EngineData::load(&root)?)?;
    let batch;
    let bytes = match options.mode {
        Mode::Serve => {
            let workers = match env::var("WF_SERVER_THREADS") {
                Ok(value) => value.parse::<usize>().ok().filter(|v| *v > 0)
                    .ok_or("WF_SERVER_THREADS 必须为正整数")?,
                Err(env::VarError::NotPresent) => 2,
                Err(error) => return Err(error.into()),
            };
            return service.serve(SocketAddr::new(options.host, options.port), workers);
        }
        Mode::Probe => service.health_json(),
        Mode::Dump => service.catalog_json(),
        Mode::SimulateBatch(path) => {
            let input = serde_json::from_reader(std::io::BufReader::new(std::fs::File::open(path)?))?;
            batch = serde_json::to_vec(&service.simulate_batch(input)?)?;
            &batch
        }
    };
    let mut stdout = io::stdout().lock();
    stdout.write_all(bytes)?;
    stdout.write_all(b"\n")?;
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("wfcore-server 启动失败：{error}");
            ExitCode::FAILURE
        }
    }
}
