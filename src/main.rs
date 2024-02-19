
#![deny(unsafe_code)]
#![deny(clippy::all)]
#![deny(clippy::missing_docs_in_private_items)]
#![warn(clippy::pedantic)]

//! Simple file server written in (safe) Rust, which allows access to incomplete files, sending new bytes when they are received.
//! For more information see `readme.adoc`

use axum::{
    body::Body,
    extract::{
        Path,
        Request,
        State
    },
    http::{
        header::{
            self,
            HeaderMap
        },
        StatusCode
    },
    Json,
    response::{
        Html,
        IntoResponse,
        Response
    },
    Router,
    routing::get,
};
use futures::stream::StreamExt;

mod video_store;
use crate::video_store::{VideoStore, Config};


/// CRUD stream server
#[tokio::main]
async fn main() {
    
    let video_store = VideoStore::new(
        Config {
            file_allocated_blocks_no: 0x100_000,
            file_preferred_block_size: 0x100_000,
        }
    );
    
    let app = Router::new()
        .route("/", get(show_help_page))           // `GET /` is handled by `show_help_page`
        .route("/*path", get(get_stream)           // `GET` is handled by `get_stream`
                          .put(put_stream)         // `PUT` is handled by `put_stream`
                          .delete(delete_stream)   // `DELETE` is handled by `delete_stream`
                          .fallback(list_streams)) // as a fallback (`LIST`) use `list_streams`
        .with_state(video_store);                  // as a state object use video_store
    
    // Start serving
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

/// Returns home page content (`/`)
async fn show_help_page() -> Html<&'static str> {
    let content = "<h1>RustStreamServer</h1>\
<p>Individual streams (`/{streamName}`) accept methods GET, PUT, DELETE</p>\
<p>To list all streams matching a prefix, use custom HTTP method LIST on (`/{prefix}*`)</p>";
    Html(content)
}

/// Accept a chunked file stream.
/// URL must not contain wildcard (*) and request must have Content-Type to be considered valid.
/// Subsequent PUT requests at given path overwrite the previous file, as per PUT's semantic.
async fn put_stream(
    State(video_store): State<VideoStore>,
    Path(path): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> impl IntoResponse {
    
    if path.contains('*') {
        return StatusCode::BAD_REQUEST; // path must not contain '*'!
    }
    
    // Check content type is present, create a VideoFile in the VideoStore
    let content_type = headers.get(header::CONTENT_TYPE).and_then(|e| e.to_str().ok());
    let write_handle = match content_type {
        None => return StatusCode::BAD_REQUEST, // must have content type header!
        Some(content_type) => {
            video_store.create(path, content_type.to_string())
        }
    };
    
    // As long as chunks are incoming, write them to provided handle
    let preferred_block_size = video_store.config().file_preferred_block_size;
    let mut stream = body.into_data_stream();
    let mut new_block = Vec::with_capacity(preferred_block_size);
    while let Some(polled) = stream.next().await {
        match polled {
            Ok(bytes) => {
                new_block.extend_from_slice(&bytes);
                if new_block.len() >= preferred_block_size {
                    write_handle.broadcast(new_block.clone()).await.unwrap();
                    new_block = Vec::with_capacity(preferred_block_size);
                }
            },
            Err(_err) => {
                if !new_block.is_empty() {
                    write_handle.broadcast(new_block.clone()).await.unwrap();
                }
                write_handle.close();
                // INFO: I'm really not sure what the correct behaviour should be in case of error
                // delete_stream(State(video_store), Path(path));
                return StatusCode::BAD_REQUEST;
            },
        }
    }
    if !new_block.is_empty() {
        write_handle.broadcast(new_block.clone()).await.unwrap();
    }
    write_handle.close();
    
    StatusCode::CREATED
}

/// Retrieves a stored file stream. In case the file is not yet fully uploaded,
///   the connection will be kept alive and new chunk will be sent as received.
async fn get_stream(
    State(video_store): State<VideoStore>,
    Path(path): Path<String>,
) -> impl IntoResponse {
    
    // If desired file is present, read its stream
    match video_store.get(&path) {
        None => {
            StatusCode::NOT_FOUND.into_response()
        },
        Some(video_file) => {
            Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", video_file.content_type())
                .body(Body::from_stream(
                    video_file.data_stream().map(Ok::<Vec<u8>, ::std::io::Error>)
                ))
                .unwrap()
        }
    }
}

/// Delete stored stream if present and return 204 (`NO_CONTENT`),
//    otherwise return 404 (NOT_FOUND), as per DELETE's semantic.
async fn delete_stream(
    State(video_store): State<VideoStore>,
    Path(path): Path<String>
) -> impl IntoResponse {
    
    match video_store.remove(&path) {
        None => StatusCode::NOT_FOUND,
        Some(()) => StatusCode::NO_CONTENT,
    }
}

/// List streams with given prefix
async fn list_streams(
    State(video_store): State<VideoStore>,
    Path(path): Path<String>,
    req: Request,
) -> impl IntoResponse {
    
    if req.method() != "LIST" {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    
    match path.find('*') {
        Some(pos) if pos != path.len()-1 => return StatusCode::BAD_REQUEST.into_response(),
        Some(_) | None => {},
    }
    
    Json(video_store.list(&path)).into_response()
}

// Hic sunt tests:

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::{
            hash_map::RandomState,
            HashSet,
        },
        sync::Arc,
    };
    use futures::task::Poll;
    use http_body_util::BodyExt;
    use serde_json::Value;
    use parking_lot::RwLock;


#[tokio::test]
async fn test_put_at_wildcard_400s() {
    
    let state = State(VideoStore::new(Config {
        file_allocated_blocks_no: 0x10,
        file_preferred_block_size: 0x10,
    }));
    let path = Path("invalid*path".into());
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, "text/plain".parse().unwrap());
    let body = Body::new("The quick brown fox jumps over the lazy dog".to_string());
    
    let response = put_stream(state.clone(), path, headers, body).await.into_response();
    
    // Test that PUT request at path containing wildcard ('*') returns BAD_REQUEST, nothing is inserted
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(state.get("invalid*path").is_none());
}

#[tokio::test]
async fn test_put_without_content_type_400s() {
    
    let state = State(VideoStore::new(Config {
        file_allocated_blocks_no: 0x10,
        file_preferred_block_size: 0x10,
    }));
    let path = Path("valid_path".into());
    let headers = HeaderMap::new();
    let body = Body::new("The quick brown fox jumps over the lazy dog".to_string());
    
    let response = put_stream(state.clone(), path, headers, body).await.into_response();
    
    // Test that PUT request with no Content-Type header returns BAD_REQUEST, nothing is inserted
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(state.get("valid_path").is_none());
}

#[tokio::test]
async fn test_put_works() {
    
    let state = State(VideoStore::new(Config {
        file_allocated_blocks_no: 0x10,
        file_preferred_block_size: 0x10,
    }));
    let path = Path("valid_path".into());
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, "text/plain".parse().unwrap());
    let body = Body::from_stream(futures::stream::iter(
                    "The quick brown fox jumps over the lazy dog"
                    .split(" ").map(Ok::<&str, std::io::Error>)));
    
    let response = put_stream(state.clone(), path, headers, body).await.into_response();
    
    // Test that valid PUT request goes through
    assert_eq!(response.status(), StatusCode::CREATED);
    assert!(state.get("valid_path").is_some());
    
    let file = state.get("valid_path").unwrap();
    assert_eq!(file.content_type(), "text/plain");
    
    let mut stored_data = "".to_string();
    let mut stream = file.data_stream();
    while let Some(v) = stream.next().await {
        stored_data += std::str::from_utf8(&v).unwrap();
    }
    assert_eq!(stored_data, "The quick brown fox jumps over the lazy dog".replace(" ", ""));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_put_is_immediately_readable() {
    
    let video_store = VideoStore::new(Config {
        file_allocated_blocks_no: 0x10,
        file_preferred_block_size: 0x10,
    });
    let state = State(video_store.clone());
    let path = Path("valid_path".into());
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, "text/plain".parse().unwrap());
    
    let sync = Arc::new(RwLock::new(0));
    let sync_clone = sync.clone();
    let body = Body::from_stream(futures::stream::poll_fn(
        move |_| -> Poll<Option<Result<String, std::io::Error>>> {
            let phase = *sync_clone.read();
            match phase {
                0 => {
                    *sync_clone.write() = 1;
                    Poll::Ready(Some(Ok("The quick brown fox".to_string())))
                },
                1 | 2 => {
                    loop {
                        if *sync_clone.read() == 2 {
                            break;
                        }
                    }
                    *sync_clone.write() = 3;
                    Poll::Ready(Some(Ok("jumps over the lazy dog".to_string())))
                },
                3 => {
                    Poll::Ready(None)
                },
                _ => unreachable!(),
            }
        }
    ));
    
    let response_thread = tokio::task::spawn(async move {
        put_stream(state, path, headers, body).await.into_response()
    });
    
    // Test file inserted by PUT is available after first bytes are inserted
    loop {
        if let Some(file) = video_store.get("valid_path") {
            let mut stream = file.data_stream();
            assert_eq!(file.content_type(), "text/plain");
            match stream.next().await {
                Some(e) => {
                    assert_eq!(e, "The quick brown fox".to_string().as_bytes());
                    *sync.write() = 2;
                },
                None => unreachable!(),
            }
            match stream.next().await {
                Some(e) => assert_eq!(e, "jumps over the lazy dog".to_string().as_bytes()),
                None => unreachable!(),
            }
            break;
        }
    }
    
    let response = response_thread.await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_put_force_stream_sleep() {
    
    let video_store = VideoStore::new(Config {
        file_allocated_blocks_no: 0x10,
        file_preferred_block_size: 0x10,
    });
    let state = State(video_store.clone());
    let path = Path("valid_path".into());
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, "text/plain".parse().unwrap());
    
    let sync1 = Arc::new(RwLock::new(0));
    let sync1_clone = sync1.clone();
    let sync2 = Arc::new(RwLock::new(false));
    let sync2_clone = sync2.clone();
    let body = Body::from_stream(futures::stream::poll_fn(
        move |_| -> Poll<Option<Result<String, std::io::Error>>> {
            let phase = *sync1_clone.read();
            match phase {
                0 => {
                    *sync1_clone.write() = 1;
                    Poll::Ready(Some(Ok("The quick brown fox".to_string())))
                },
                1 | 2 => {
                    loop {
                        if *sync1_clone.read() == 2 {
                            break;
                        }
                    }
                    *sync1_clone.write() = 3;
                    Poll::Ready(Some(Ok("jumps over the lazy dog".to_string())))
                },
                3 => {
                    Poll::Ready(None)
                },
                _ => unreachable!(),
            }
        }
    ));
    
    let writer_thread = tokio::task::spawn(async move {
        put_stream(state, path, headers, body).await.into_response()
    });
    
    // Test file inserted by PUT is available after first bytes are inserted,
    //   then readable until end even when the stream has to sleep and wake up
    let reader_thread = tokio::task::spawn(async move {
        loop {
            if let Some(file) = video_store.get("valid_path") {
                let mut stream = file.data_stream();
                assert_eq!(file.content_type(), "text/plain");
                match stream.next().await {
                    Some(e) => {
                        assert_eq!(e, "The quick brown fox".to_string().as_bytes());
                        *sync2.write() = true;
                    },
                    None => unreachable!(),
                }
                match stream.next().await { // This await causes stream to sleep
                    Some(e) => assert_eq!(e, "jumps over the lazy dog".to_string().as_bytes()),
                    None => unreachable!(),
                }
                break;
            }
        }
    });
    
    loop {
        let phase = *sync2_clone.read();
        if phase {
            std::thread::sleep(std::time::Duration::from_millis(2500));
            *sync1.write() = 2;
            break;
        }
    };
    
    let response = writer_thread.await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let _ = reader_thread.await.unwrap();
}

#[tokio::test]
async fn test_get_nonexistent_404s() {
    
    let state = State(VideoStore::new(Config {
        file_allocated_blocks_no: 0x10,
        file_preferred_block_size: 0x10,
    }));
    let path = Path("nonexistent_path".into());
    
    let response = get_stream(state.clone(), path).await.into_response();
    
    // Test that GET request to non-existent path returns NOT_FOUND
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_get_returns_completed_file() {
    
    let state = State(VideoStore::new(Config {
        file_allocated_blocks_no: 0x10,
        file_preferred_block_size: 0x10,
    }));
    
    let handle = state.create("existing_path".to_string(), "text/plain".to_string());
    for e in "The quick brown fox jumps over the lazy dog"
            .split(" ").map(|e| e.to_string().into_bytes()) {
        handle.broadcast(e).await.unwrap();
    }
    handle.close();
    
    let path = Path("existing_path".into());
    
    let response = get_stream(state.clone(), path).await.into_response();
    
    // Test that GET request for finished file works
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.into_body().collect().await.unwrap().to_bytes(),
               "The quick brown fox jumps over the lazy dog".replace(" ", ""));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_get_returns_incomplete_file() {
    
    let video_store = VideoStore::new(Config {
        file_allocated_blocks_no: 0x10,
        file_preferred_block_size: 0x10,
    });
    let state = State(video_store.clone());
    
    let path = Path("valid_path".into());
    let handle = state.create("valid_path".to_string(), "text/plain".to_string());
    handle.broadcast("The quick brown fox".to_string().into_bytes()).await.unwrap();
    
    let sync = Arc::new(RwLock::new(false));
    let sync_clone = sync.clone();
    
    // Test GET returns available parts of incomplete file
    let reader_thread = tokio::task::spawn(async move {
        let response = get_stream(state, path).await.into_response();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get(header::CONTENT_TYPE).unwrap().to_str().unwrap(),
                   "text/plain");
        let mut stream = response.into_body().into_data_stream();
        match stream.next().await {
            Some(e) => {
                assert_eq!(e.unwrap(), "The quick brown fox".to_string().as_bytes());
                *sync_clone.write() = true;
            },
            None => unreachable!(),
        }
        match stream.next().await {
            Some(e) => assert_eq!(e.unwrap(), "jumps over the lazy dog".to_string().as_bytes()),
            None => unreachable!(),
        }
    });
    
    loop {
        let can_push_more = *sync.read();
        if can_push_more {
            handle.broadcast("jumps over the lazy dog".to_string().into_bytes()).await.unwrap();
            handle.close();
            break;
        }
    }
    
    let _ = reader_thread.await.unwrap();
}

#[tokio::test]
async fn test_get_works_repeatedly() {
    
    let state = State(VideoStore::new(Config {
        file_allocated_blocks_no: 0x10,
        file_preferred_block_size: 0x10,
    }));
    
    let path = Path("existing_path".to_string());
    let handle = state.create("existing_path".to_string(), "text/plain".to_string());
    for e in "The quick brown fox jumps over the lazy dog"
            .split(" ").map(|e| e.to_string().into_bytes()) {
        handle.broadcast(e).await.unwrap();
    }
    handle.close();
    
    // Test that both GET requests get correct result
    let response1 = get_stream(state.clone(), Path(path.clone())).await.into_response();
    assert_eq!(response1.status(), StatusCode::OK);
    assert_eq!(response1.into_body().collect().await.unwrap().to_bytes(),
               "The quick brown fox jumps over the lazy dog".replace(" ", ""));
    
    let response2 = get_stream(state.clone(), path).await.into_response();
    assert_eq!(response2.status(), StatusCode::OK);
    assert_eq!(response2.into_body().collect().await.unwrap().to_bytes(),
               "The quick brown fox jumps over the lazy dog".replace(" ", ""));
}

#[tokio::test]
async fn test_delete_404s_when_not_found() {
    
    let state = State(VideoStore::new(Config {
        file_allocated_blocks_no: 0x10,
        file_preferred_block_size: 0x10,
    }));
    let path = Path("nonexistent_path".into());
    
    let response = delete_stream(state.clone(), path).await.into_response();
    
    // Test that DELETE request to non-existent path returns NOT_FOUND
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_delete_deletes() {
    
    let state = State(VideoStore::new(Config {
        file_allocated_blocks_no: 0x10,
        file_preferred_block_size: 0x10,
    }));
    
    let path = Path("existing_path".to_string());
    let handle = state.create("existing_path".to_string(), "text/plain".to_string());
    for e in "The quick brown fox jumps over the lazy dog"
            .split(" ").map(|e| e.to_string().into_bytes()) {
        handle.broadcast(e).await.unwrap();
    }
    
    let response = delete_stream(state.clone(), path).await.into_response();
    
    // Test that DELETE request deletes content at given path
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(state.get("existing_path").is_none());
}

#[tokio::test]
async fn test_list_400s_on_nonlast_wildcard() {
    
    let state = State(VideoStore::new(Config {
        file_allocated_blocks_no: 0x10,
        file_preferred_block_size: 0x10,
    }));
    let path = Path("part1*part2".into());
    let request = Request::builder()
        .method("LIST")
        .uri("https://127.0.0.1:3000/*")
        .body(Body::empty())
        .unwrap();
    
    let response = list_streams(state.clone(), path, request).await.into_response();
    
    // Test that LIST with to non-last wildcard returns BAD_REQUEST
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_list_lists_all_prefix() {
    
    let state = State(VideoStore::new(Config {
        file_allocated_blocks_no: 0x10,
        file_preferred_block_size: 0x10,
    }));
    
    let file1 = state.create("a_path".to_string(), "text/plain".to_string());
    let file2 = state.create("b_path1".to_string(), "text/plain".to_string());
    let file3 = state.create("b_path2".to_string(), "text/plain".to_string());
    
    for e in "The quick brown fox jumps over the lazy dog"
            .split(" ").map(|e| e.to_string().into_bytes()) {
        file1.broadcast(e.clone()).await.unwrap();
        file2.broadcast(e.clone()).await.unwrap();
        file3.broadcast(e).await.unwrap();
    }
    
    let path = Path("*".into());
    let request = Request::builder()
        .method("LIST")
        .uri("https://127.0.0.1:3000/*")
        .body(Body::empty())
        .unwrap();
    
    // Test LIST shows everything on /*
    let response = list_streams(state.clone(), path, request).await.into_response();
    assert_eq!(StatusCode::OK, response.status());
    
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let parsed_body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(HashSet::<_, RandomState>::from_iter(parsed_body.as_array().unwrap().iter()
                    .map(|e| e.as_str().unwrap())),
               HashSet::from_iter(vec!["a_path", "b_path1", "b_path2"].iter().cloned()));
}

#[tokio::test]
async fn test_list_lists_some_prefix() {
    
    let state = State(VideoStore::new(Config {
        file_allocated_blocks_no: 0x10,
        file_preferred_block_size: 0x10,
    }));
    
    let file1 = state.create("a_path".to_string(), "text/plain".to_string());
    let file2 = state.create("b_path1".to_string(), "text/plain".to_string());
    let file3 = state.create("b_path2".to_string(), "text/plain".to_string());
    
    for e in "The quick brown fox jumps over the lazy dog"
            .split(" ").map(|e| e.to_string().into_bytes()) {
        file1.broadcast(e.clone()).await.unwrap();
        file2.broadcast(e.clone()).await.unwrap();
        file3.broadcast(e).await.unwrap();
    }
    
    let path = Path("b_*".into());
    let request = Request::builder()
        .method("LIST")
        .uri("https://127.0.0.1:3000/b_*")
        .body(Body::empty())
        .unwrap();
    
    // Test LIST shows some prefix matches on /b_*
    let response = list_streams(state.clone(), path, request).await.into_response();
    assert_eq!(StatusCode::OK, response.status());
    
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let parsed_body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(HashSet::<_, RandomState>::from_iter(parsed_body.as_array().unwrap().iter()
                    .map(|e| e.as_str().unwrap())),
               HashSet::from_iter(vec!["b_path1", "b_path2"].iter().cloned()));
}

#[tokio::test]
async fn test_list_lists_empty_prefix() {
    
    let state = State(VideoStore::new(Config {
        file_allocated_blocks_no: 0x10,
        file_preferred_block_size: 0x10,
    }));
    
    let file1 = state.create("a_path".to_string(), "text/plain".to_string());
    let file2 = state.create("b_path1".to_string(), "text/plain".to_string());
    let file3 = state.create("b_path2".to_string(), "text/plain".to_string());
    
    for e in "The quick brown fox jumps over the lazy dog"
            .split(" ").map(|e| e.to_string().into_bytes()) {
        file1.broadcast(e.clone()).await.unwrap();
        file2.broadcast(e.clone()).await.unwrap();
        file3.broadcast(e).await.unwrap();
    }
    
    let path = Path("c_*".into());
    let request = Request::builder()
        .method("LIST")
        .uri("https://127.0.0.1:3000/c_*")
        .body(Body::empty())
        .unwrap();
    
    // Test LIST shows no prefix matches on /c_*
    let response = list_streams(state.clone(), path, request).await.into_response();
    assert_eq!(StatusCode::OK, response.status());
    
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let parsed_body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(HashSet::<_, RandomState>::from_iter(parsed_body.as_array().unwrap().iter()
                    .map(|e| e.as_str().unwrap())),
               HashSet::from_iter(vec![].iter().cloned()));
}

#[tokio::test]
async fn test_list_lists_one_exact() {
    
    let state = State(VideoStore::new(Config {
        file_allocated_blocks_no: 0x10,
        file_preferred_block_size: 0x10,
    }));
    
    let file1 = state.create("a_path".to_string(), "text/plain".to_string());
    let file2 = state.create("b_path1".to_string(), "text/plain".to_string());
    let file3 = state.create("b_path2".to_string(), "text/plain".to_string());
    
    for e in "The quick brown fox jumps over the lazy dog"
            .split(" ").map(|e| e.to_string().into_bytes()) {
        file1.broadcast(e.clone()).await.unwrap();
        file2.broadcast(e.clone()).await.unwrap();
        file3.broadcast(e).await.unwrap();
    }
    
    let path = Path("b_path1".into());
    let request = Request::builder()
        .method("LIST")
        .uri("https://127.0.0.1:3000/b_path1")
        .body(Body::empty())
        .unwrap();
    
    // Test LIST shows one exact match
    let response = list_streams(state.clone(), path, request).await.into_response();
    assert_eq!(StatusCode::OK, response.status());
    
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let parsed_body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(HashSet::<_, RandomState>::from_iter(parsed_body.as_array().unwrap().iter()
                    .map(|e| e.as_str().unwrap())),
               HashSet::from_iter(vec!["b_path1"].iter().cloned()));
}

#[tokio::test]
async fn test_list_lists_empty_exact() {
    
    let state = State(VideoStore::new(Config {
        file_allocated_blocks_no: 0x10,
        file_preferred_block_size: 0x10,
    }));
    
    let file1 = state.create("a_path".to_string(), "text/plain".to_string());
    let file2 = state.create("b_path1".to_string(), "text/plain".to_string());
    let file3 = state.create("b_path2".to_string(), "text/plain".to_string());
    
    for e in "The quick brown fox jumps over the lazy dog"
            .split(" ").map(|e| e.to_string().into_bytes()) {
        file1.broadcast(e.clone()).await.unwrap();
        file2.broadcast(e.clone()).await.unwrap();
        file3.broadcast(e).await.unwrap();
    }
    
    let path = Path("c_path".into());
    let request = Request::builder()
        .method("LIST")
        .uri("https://127.0.0.1:3000/c_path")
        .body(Body::empty())
        .unwrap();
    
    // Test LIST shows empty when there is no exact match
    let response = list_streams(state.clone(), path, request).await.into_response();
    assert_eq!(StatusCode::OK, response.status());
    
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let parsed_body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(HashSet::<_, RandomState>::from_iter(parsed_body.as_array().unwrap().iter()
                    .map(|e| e.as_str().unwrap())),
               HashSet::from_iter(vec![].iter().cloned()));
}
}
