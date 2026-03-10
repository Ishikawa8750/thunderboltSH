use std::{
    path::{Path, PathBuf},
    time::UNIX_EPOCH
};

use axum::{
    body::Body,
    extract::{Multipart, Query, State},
    http::{header, HeaderMap, Request, StatusCode},
    middleware::{from_fn_with_state, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router
};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

#[derive(Deserialize)]
pub struct PathQuery {
    path: String
}

#[derive(Deserialize)]
pub struct MoveQuery {
    from: String,
    to: String
}

#[derive(Serialize)]
pub struct FsEntry {
    name: String,
    is_dir: bool,
    size: u64,
    mtime: u64
}

#[derive(Serialize)]
pub struct FsStat {
    exists: bool,
    is_dir: bool,
    size: Option<u64>,
    mtime: Option<u64>
}

#[derive(Clone)]
pub struct ApiAuthConfig {
    token: Option<String>
}

pub fn router(api_token: Option<String>) -> Router {
    let auth = ApiAuthConfig { token: api_token };

    Router::new()
        .route("/api/fs/list", get(list_directory))
        .route("/api/fs/stat", get(stat_path))
        .route("/api/fs/download", get(download_file))
        .route("/api/fs/delete", post(delete_path))
        .route("/api/fs/move", post(move_path))
        .route("/api/fs/upload", post(upload_file))
        .layer(from_fn_with_state(auth, require_token))
}

async fn require_token(
    State(auth): State<ApiAuthConfig>,
    headers: HeaderMap,
    request: Request<Body>,
    next: Next
) -> Response {
    let Some(token) = auth.token else {
        return next.run(request).await;
    };

    let provided = headers
        .get("x-openbolt-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();

    if provided == token {
        next.run(request).await
    } else {
        (StatusCode::UNAUTHORIZED, "invalid token").into_response()
    }
}

async fn move_path(Query(query): Query<MoveQuery>) -> Result<String, AppHttpError> {
    let from = decode_path(&query.from)?;
    let to = decode_path(&query.to)?;

    if !from.exists() {
        return Ok("ok".to_string());
    }

    if let Some(parent) = to.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    tokio::fs::rename(from, to).await?;
    Ok("ok".to_string())
}

async fn delete_path(Query(query): Query<PathQuery>) -> Result<String, AppHttpError> {
    let path = decode_path(&query.path)?;

    if !path.exists() {
        return Ok("ok".to_string());
    }

    let metadata = tokio::fs::metadata(&path).await?;
    if metadata.is_dir() {
        tokio::fs::remove_dir_all(path).await?;
    } else {
        tokio::fs::remove_file(path).await?;
    }

    Ok("ok".to_string())
}

async fn stat_path(Query(query): Query<PathQuery>) -> Result<Json<FsStat>, AppHttpError> {
    let path = decode_path(&query.path)?;
    if !path.exists() {
        return Ok(Json(FsStat {
            exists: false,
            is_dir: false,
            size: None,
            mtime: None
        }));
    }

    let metadata = tokio::fs::metadata(path).await?;
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|v| v.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs());

    Ok(Json(FsStat {
        exists: true,
        is_dir: metadata.is_dir(),
        size: Some(metadata.len()),
        mtime
    }))
}

async fn list_directory(Query(query): Query<PathQuery>) -> Result<Json<Vec<FsEntry>>, AppHttpError> {
    let path = decode_path(&query.path)?;
    let mut entries = tokio::fs::read_dir(path).await?;
    let mut result = Vec::new();

    while let Some(entry) = entries.next_entry().await? {
        let metadata = entry.metadata().await?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|v| v.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or_default();

        result.push(FsEntry {
            name: entry.file_name().to_string_lossy().to_string(),
            is_dir: metadata.is_dir(),
            size: metadata.len(),
            mtime: modified
        });
    }

    Ok(Json(result))
}

async fn download_file(Query(query): Query<PathQuery>) -> Result<Response, AppHttpError> {
    let path = decode_path(&query.path)?;
    let metadata = tokio::fs::metadata(&path).await?;
    let file = tokio::fs::File::open(path).await?;
    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    let response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, metadata.len())
        .body(body)
        .map_err(|e| AppHttpError::bad_request(e.to_string()))?;

    Ok(response)
}

async fn upload_file(Query(query): Query<PathQuery>, mut multipart: Multipart) -> Result<String, AppHttpError> {
    let dest_dir = decode_path(&query.path)?;
    tokio::fs::create_dir_all(&dest_dir).await?;

    while let Some(field) = multipart.next_field().await.map_err(AppHttpError::from)? {
        let file_name = field
            .file_name()
            .map(|v| v.to_string())
            .unwrap_or_else(|| "unnamed.bin".to_string());
        let dest_path = dest_dir.join(file_name);
        let mut file = tokio::fs::File::create(dest_path).await?;

        let mut field = field;
        while let Some(chunk) = field.chunk().await.map_err(AppHttpError::from)? {
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
    }

    Ok("ok".to_string())
}

fn decode_path(raw: &str) -> Result<PathBuf, AppHttpError> {
    let decoded = urlencoding::decode(raw).map_err(|e| AppHttpError::bad_request(e.to_string()))?;
    let path = Path::new(decoded.as_ref()).to_path_buf();

    if path.as_os_str().is_empty() {
        return Err(AppHttpError::bad_request("path is empty".to_string()));
    }

    Ok(path)
}

struct AppHttpError {
    status: StatusCode,
    message: String
}

impl AppHttpError {
    fn bad_request(message: String) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message
        }
    }
}

impl From<std::io::Error> for AppHttpError {
    fn from(value: std::io::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: value.to_string()
        }
    }
}

impl From<axum::extract::multipart::MultipartError> for AppHttpError {
    fn from(value: axum::extract::multipart::MultipartError) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: value.to_string()
        }
    }
}

impl IntoResponse for AppHttpError {
    fn into_response(self) -> Response {
        (self.status, self.message).into_response()
    }
}
