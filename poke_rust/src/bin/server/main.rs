//! HTTP API server for the PokeRust web frontend. Loads the dexes once at
//! startup and exposes battle sessions over JSON — see routes.rs for endpoints.
//! Run from `poke_rust/` so the default dex paths resolve.

mod dto;
mod mapping;
mod routes;
mod session;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::routing::{get, post};
use axum::Router;
use clap::Parser;
use tower_http::cors::CorsLayer;

use routes::AppState;
use session::Dexes;

#[derive(Parser, Debug)]
#[command(about = "PokeRust web API server")]
struct Args {
    /// Path to the showdown pokedex data
    #[arg(long, default_value = "../pokemon_info/showdownDex.txt")]
    poke_dex: String,

    /// Path to the showdown move data
    #[arg(long, default_value = "../pokemon_info/showdownMoves.txt")]
    move_dex: String,

    /// Port to bind on 127.0.0.1
    #[arg(long, default_value_t = 3001)]
    port: u16,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    // Keep the engine silent; per-battle sim options come with each request.
    let _ = poke_rust::VERBOSITY.set(0);

    let dexes = Arc::new(Dexes {
        pokemon_dex: poke_rust::state::dex_data::parse_pokemon_dex(&args.poke_dex),
        move_dex: poke_rust::state::dex_data::parse_move_dex(&args.move_dex),
    });
    println!(
        "Loaded {} Pokemon, {} moves",
        dexes.pokemon_dex.len(),
        dexes.move_dex.len()
    );

    let state = AppState {
        dexes,
        sessions: Arc::new(Mutex::new(HashMap::new())),
    };

    let app = Router::new()
        .route("/api/battles", post(routes::create_battle))
        .route(
            "/api/battles/{id}",
            get(routes::get_battle).delete(routes::delete_battle),
        )
        .route("/api/battles/{id}/commands", get(routes::get_commands))
        .route("/api/battles/{id}/turn", post(routes::submit_turn))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = format!("127.0.0.1:{}", args.port);
    println!("PokeRust server listening on http://{}", addr);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind port");
    axum::serve(listener, app).await.expect("server error");
}
