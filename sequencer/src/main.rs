use clap::Parser;

#[derive(Parser)]
struct Args {
    #[arg(short, long, env = "TOKIO_THREAD_STACK_SIZE")]
    tokio_thread_stack_size: Option<usize>,
}

pub fn main() -> anyhow::Result<()> {
    // Parse args
    let args = Args::parse();

    // Set tokio thread stack size
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(args.tokio_thread_stack_size.unwrap_or(4194304))
        .build()
        .unwrap();

    rt.block_on(sequencer::main())
}
