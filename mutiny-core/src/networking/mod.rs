pub mod socket;
pub mod websocket;

#[cfg(target_arch = "wasm32")]
pub mod proxy;

#[cfg(target_arch = "wasm32")]
pub mod ws_socket;

#[cfg(not(target_arch = "wasm32"))]
pub mod tcp_socket;

#[cfg(target_arch = "wasm32")]
pub mod ws_io;