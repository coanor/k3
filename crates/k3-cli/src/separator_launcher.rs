//! 发行包中的原生入口：按自身位置启动随包 Python，不依赖虚拟环境的绝对路径。

use std::{env, error::Error, process::Command};

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("k3-separator: {error}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<i32, Box<dyn Error>> {
    let executable = env::current_exe()?;
    let root = executable
        .parent()
        .ok_or("cannot locate package directory")?;
    let python = root.join(if cfg!(windows) {
        "runtime/python/python.exe"
    } else {
        "runtime/python/bin/python3"
    });
    if !python.is_file() {
        return Err(format!("bundled Python is missing: {}", python.display()).into());
    }
    let mut paths = vec![root.join("runtime/bin")];
    paths.extend(env::split_paths(&env::var_os("PATH").unwrap_or_default()));
    let args: Vec<_> = env::args_os().skip(1).collect();
    let has_model_dir = args
        .iter()
        .any(|arg| arg == "--model-dir" || arg.to_string_lossy().starts_with("--model-dir="));
    let mut command = Command::new(python);
    command
        .args(["-I", "-X", "utf8", "-m", "k3_separator"])
        .env("PATH", env::join_paths(paths)?)
        .env("PYTHONNOUSERSITE", "1")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env_remove("PYTHONHOME")
        .env_remove("PYTHONPATH");
    if !has_model_dir {
        command.arg("--model-dir").arg(root.join("models"));
    }
    command.args(args);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        // 保留 PID 和管道所有权，让调用方的 kill/reap 直接作用于 Python。
        Err(command.exec().into())
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use win32job::{ExtendedLimitInfo, Job};

        let mut limits = ExtendedLimitInfo::new();
        limits.limit_kill_on_job_close();
        let job = Job::create_with_limit_info(&mut limits)?;
        // 在 spawn 前加入 job，Python 及其子进程自动继承，避免分配时的竞态。
        job.assign_current_process()?;
        // job 包含当前进程；提前 drop 会终止自己并覆盖 worker 的退出码。
        // 句柄不可继承，交给进程退出时由 OS 关闭；每个 launcher 仅持有一个。
        std::mem::forget(job);
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        let status = command.status()?;
        Ok(status.code().unwrap_or(1))
    }
}
