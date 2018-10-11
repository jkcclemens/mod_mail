#![feature(box_syntax)]
#![allow(proc_macro_derive_resolution_fallback)]

#[macro_use] extern crate diesel;
#[macro_use] extern crate error_chain;

use diesel::pg::PgConnection;

use r2d2_diesel::ConnectionManager;

use serenity::Client;

mod database;
mod error;
mod listener;
mod logging;

use crate::listener::Listener;

fn main() {
  if let Err(e) = log::set_logger(&crate::logging::SimpleLogger)
    .map(|()| log::set_max_level(log::LevelFilter::Trace)) {
    eprintln!("could not set up logger: {}", e);
  }

  dotenv::dotenv().ok();

  let token = match std::env::var("MM_DISCORD_TOKEN") {
    Ok(t) => t,
    Err(_) => {
      eprintln!("Missing expected environment variable MM_DISCORD_TOKEN.");
      return;
    },
  };

  let database_url = match std::env::var("DATABASE_URL") {
    Ok(d) => d,
    Err(_) => {
      eprintln!("Missing expected environment variable DATABASE_URL.");
      return;
    },
  };

  let manager = ConnectionManager::<PgConnection>::new(database_url.as_str());
  let pool = match r2d2::Pool::builder().build(manager) {
    Ok(p) => p,
    Err(e) => {
      eprintln!("Could not establish connection to the database: {}", e);
      return;
    },
  };

  let mut client = match Client::new(&token, Listener::new(pool.clone())) {
    Ok(c) => c,
    Err(e) => {
      eprintln!("Could not create bot: {}.", e);
      return;
    },
  };
  if let Err(e) = client.start_autosharded() {
    eprintln!("Could not start bot: {}.", e);
  }
}
