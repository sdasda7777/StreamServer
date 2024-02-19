
use reqwest::Client;
use futures::stream::StreamExt;

async fn read_numbers(path: &str) {
    // Create a request with the stream as the body
    let client = Client::new();
    let response = client.get(format!("http://127.0.0.1:3000/{}", path))
                        .send()
                        .await;
    
    match response {
        Err(err) => println!("Response is {:?}", err),
        Ok(response) => {
            let mut number_iter = (0..=255).cycle();
            let mut response_bytes = response.bytes_stream();
            while let Some(chunk_res) = response_bytes.next().await {
                match chunk_res {
                    Err(err) => println!("Response Err: {:?}", err),
                    Ok(chunk) => for byte in chunk {
                        assert_eq!(byte, number_iter.next().unwrap());
                    }
                }
            }
            println!("Stream from {} ok", path);
        }
    }
}

#[tokio::main]
async fn main() {
    
    let paths = vec!["qwer", "asdf", "yxcv", "uiop", "hjkl", "vbnm"];
    let mut tasks = Vec::with_capacity(paths.len());
    
    for path in paths {
        for _ in 0..10 {
            tasks.push(tokio::spawn(async move {
                read_numbers(path).await
            }));
        }
    }
    
    for task in tasks {
        task.await.unwrap();
    }
}
