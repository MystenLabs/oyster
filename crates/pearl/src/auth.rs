use tonic::{Request, Status};

/// Interceptor that checks for a shared secret in the `authorization` metadata header.
pub fn check_service_secret(
    expected_secret: String,
) -> impl Fn(Request<()>) -> Result<Request<()>, Status> + Clone {
    let expected = format!("Bearer {expected_secret}");
    move |req: Request<()>| {
        let auth = req
            .metadata()
            .get("authorization")
            .and_then(|v| v.to_str().ok());

        match auth {
            Some(val) if val == expected => Ok(req),
            _ => Err(Status::unauthenticated("invalid or missing service secret")),
        }
    }
}
