pub fn main() -> anyhow::Result<()> {
    // Get the environment variable
    let tokio_thread_stack_size = std::env::var("TOKIO_THREAD_STACK_SIZE")
        .unwrap_or("4194304".to_string())
        .parse::<usize>()
        .unwrap_or(4194304);

    // Set tokio thread stack size
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(tokio_thread_stack_size)
        .build()
        .unwrap();

    rt.block_on(sequencer::main())
}
