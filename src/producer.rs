
use reqwest::{Body, Client};
use futures::StreamExt;

async fn send_numbers(path: &str) {
    let stream = futures::stream::iter((0..=255).cycle())
                                .chunks(4_095).take(500_000)
                                .map(Ok::<Vec<u8>, ::std::io::Error>);
            let body = Body::wrap_stream(stream);

            // Create a request with the stream as the body
            let client = Client::new();
            let response = client.put(format!("http://127.0.0.1:3000/{}", path))
                                .header("Transfer-Encoding", "chunked")
                                .header("Content-Type", "text/plain")
                                .body(body)
                                .send()
                                .await;
            println!("{:?}", response);
}

#[tokio::main]
async fn main() {
    
    let paths = vec!["qwer", "asdf", "yxcv", "uiop", "hjkl", "vbnm"];
    let mut tasks = Vec::with_capacity(paths.len());
    
    for path in paths {
        tasks.push(tokio::spawn(async move {
            send_numbers(path).await
        }));
    }
    
    for task in tasks {
        task.await.unwrap();
    }
}
