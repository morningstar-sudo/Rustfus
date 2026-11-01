mod service;

fn main() {
    if let Err(error) = run() {
        eprintln!("rustfus-worker: terminated: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(windows)]
    {
        if let Err(error) = rustfus_windows::cleanup_stale_temporary_directories() {
            eprintln!("rustfus-worker: could not clean stale WIM temporary directories: {error}");
        }
        let args = std::env::args().collect::<Vec<_>>();
        let pipe_name = option_value(&args, "--pipe");
        let token = option_value(&args, "--token");
        if let (Some(pipe_name), Some(token)) = (pipe_name, token) {
            let pipe = rustfus_windows::connect_named_pipe(&pipe_name)?;
            return service::run_pipe(pipe, token);
        }
    }
    service::run_stdio()
}

#[cfg(windows)]
fn option_value(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}
