use std::{
    env, fs,
    io::{self, Write},
    process, thread,
    time::Duration,
};

fn main() {
    if env::args().any(|arg| arg == "--descendant") {
        thread::sleep(Duration::from_secs(60));
        return;
    }
    let args: Vec<_> = env::args().skip(1).collect();
    assert_eq!(&args[..5], ["-I", "-X", "utf8", "-m", "k3_separator"]);
    assert!(env::var_os("PYTHONHOME").is_none());
    assert!(env::var_os("PYTHONPATH").is_none());
    if let Ok(pid_file) = env::var("K3_TEST_PID_FILE") {
        #[cfg(windows)]
        let descendant = std::process::Command::new(env::current_exe().unwrap())
            .arg("--descendant")
            .spawn()
            .unwrap();
        #[cfg(windows)]
        let pids = format!("{} {}", process::id(), descendant.id());
        #[cfg(unix)]
        let pids = process::id().to_string();
        fs::write(pid_file, pids).unwrap();
        println!("{}", process::id());
        io::stdout().flush().unwrap();
        thread::sleep(Duration::from_secs(60));
    } else {
        println!(
            "{}",
            args[args.iter().position(|arg| arg == "--model-dir").unwrap() + 1]
        );
        process::exit(42);
    }
}
