use llrt_modules::module_builder::ModuleBuilder;
use rquickjs::{AsyncContext, AsyncRuntime, CatchResultExt, Module};

/// Runs a JS module with LLRT's Node/WinterTC APIs (fetch, timers, fs, ...).
pub async fn run_js(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let source = std::fs::read_to_string(path)?;
    let runtime = AsyncRuntime::new()?;
    let (resolver, loader, globals) = ModuleBuilder::default().build();
    runtime.set_loader((resolver,), (loader,)).await;
    let context = AsyncContext::full(&runtime).await?;

    context
        .async_with(async |ctx| {
            globals
                .attach(&ctx)
                .catch(&ctx)
                .map_err(|e| e.to_string())?;
            let promise = Module::evaluate(ctx.clone(), path, source)
                .catch(&ctx)
                .map_err(|e| e.to_string())?;
            promise
                .into_future::<()>()
                .await
                .catch(&ctx)
                .map_err(|e| e.to_string())
        })
        .await?;
    // Wait for pending timers, fetches and other spawned jobs.
    runtime.idle().await;
    Ok(())
}
