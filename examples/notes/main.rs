//! A JSON notes API on Turso. Run with `cargo run --example notes`.

mod app;

#[autumn_web::main]
async fn main() {
    autumn_web::app()
        .plugin(app::plugin())
        .routes(app::routes())
        .run()
        .await;
}
