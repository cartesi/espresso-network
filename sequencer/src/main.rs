pub fn main() -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(4194304)
        .build()
        .unwrap();

    rt.block_on(sequencer::main())
}
