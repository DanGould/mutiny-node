use futures::{channel::mpsc, SinkExt, StreamExt};
use futures_util::{io, AsyncRead, AsyncWrite};
use futures_util::stream::Stream;
use gloo_net::websocket::{Message, futures::WebSocket};
use std::pin::Pin;
use std::task::{Context, Poll};
//use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::utils;

pub struct WsIo {
    incoming: mpsc::UnboundedReceiver<Message>,
    outgoing: mpsc::UnboundedSender<Message>,
    read_buffer: Vec<u8>,
}

impl WsIo {
    pub fn new(ws: WebSocket) -> Self {
        let (outgoing, write) = mpsc::unbounded();
        let (read, incoming) = mpsc::unbounded();
        let (mut sink, mut source) = ws.split();
        utils::spawn(async move {
            while let Some(msg) = source.next().await {
                match msg {
                    Ok(Message::Text(_)) => {} // Ignore text messages
                    Ok(Message::Bytes(data)) => {
                        let _ = read.unbounded_send(Message::Bytes(data));
                    }
                    _ => break,
                }
            }
        });

        utils::spawn(async move {
            let mut outgoing = write;
            while let Some(msg) = outgoing.next().await {
                let _ = sink.send(msg);
            }
            let _ = sink.close(); // TODO test that this actualy works
        });

        WsIo {
            incoming,
            outgoing,
            read_buffer: Vec::new(),
        }
    }
}

impl AsyncRead for WsIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        if !self.read_buffer.is_empty() {
            let len = std::cmp::min(buf.len(), self.read_buffer.len());
            buf[..len].copy_from_slice(&self.read_buffer[..len]);
            self.read_buffer.drain(..len);
            return Poll::Ready(Ok(len));
        }

        match Pin::new(&mut self.incoming).poll_next(cx) {
            Poll::Ready(Some(Message::Bytes(data))) => {
                let len = std::cmp::min(buf.len(), data.len());
                buf[..len].copy_from_slice(&data[..len]);
                if data.len() > len {
                    self.read_buffer.extend_from_slice(&data[len..]);
                }
                Poll::Ready(Ok(len))
            }
            Poll::Ready(Some(_)) => Poll::Pending, // Ignore non-binary messages
            Poll::Ready(None) => Poll::Ready(Ok(0)), // End of stream, no data read
            Poll::Pending => Poll::Pending,
        }
    }
}

impl AsyncWrite for WsIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.outgoing.poll_ready(cx) {
            Poll::Ready(Ok(())) => {
                let _ = self.outgoing.start_send(Message::Bytes(data.to_vec()));
                Poll::Ready(Ok(data.len()))
            }
            Poll::Ready(Err(_)) => Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "WebSocket send channel closed",
            ))),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        Poll::Ready(Ok(())) // Nothing to flush in WebSocket context
    }

    fn poll_close(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(self.outgoing.close_channel()))
    }
}

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use gloo_net::websocket::futures::WebSocket;
    use wasm_bindgen_test::{wasm_bindgen_test as test};

    #[test]
    async fn test_ws_io() {
        use futures_util::{AsyncReadExt, AsyncWriteExt};
        assert!(true);
        // // DANGER! TODO get from &self config, do not get config directly from PAYJOIN_DIR ohttp-gateway
        // // That would reveal IP address
        // let tls_connector = {
        //     let root_store = futures_rustls::rustls::RootCertStore {
        //         roots: webpki_roots::TLS_SERVER_ROOTS.iter().cloned().collect(),
        //     };
            
        //     let config = futures_rustls::rustls::ClientConfig::builder()
        //         .with_root_certificates(root_store)
        //         .with_no_client_auth();
        //     futures_rustls::TlsConnector::from(Arc::new(config))
        // };

        // let domain = futures_rustls::rustls::pki_types::ServerName::try_from("payjo.in")
        //         .map_err(|_| {
        //             std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid dnsname")
        //         })
        //         .unwrap()
        //         .to_owned();

        // let ws = WebSocket::open(&format!("ws://127.0.0.1:3030")).unwrap();
        // let ws_io = crate::networking::ws_io::WsIo::new(ws);
        // let mut tls_stream = tls_connector.connect(domain, ws_io).await.unwrap();
        // let ohttp_keys_req = b"GET /ohttp-keys HTTP/1.1\r\nHost: payjo.in\r\nConnection: close\r\n\r\n";
        // tls_stream.write_all(ohttp_keys_req).await.unwrap();
        // tls_stream.flush().await.unwrap();
        // let mut ohttp_keys = Vec::new();
        // tls_stream.read_to_end(&mut ohttp_keys).await.unwrap();
        // let ohttp_keys_base64 = base64::encode(ohttp_keys);
        // println!("{}", &ohttp_keys_base64);
    }
}