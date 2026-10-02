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
    let status = command.args(args).status()?;
    Ok(status.code().unwrap_or(1))
}
