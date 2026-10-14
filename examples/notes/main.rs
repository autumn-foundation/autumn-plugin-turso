//! A JSON notes API on Turso. Run with `cargo run --example notes`.
//!
//! The example has no authentication. Do not deploy it as is.

mod app;

#[autumn_web::main]
async fn main() {
    autumn_web::app()
        .plugin(app::plugin())
        .routes(app::routes())
        .run()
        .await;
}
