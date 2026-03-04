use subtle::ConstantTimeEq;
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
            Some(val) if val.as_bytes().ct_eq(expected.as_bytes()).into() => Ok(req),
            _ => Err(Status::unauthenticated("invalid or missing service secret")),
        }
    }
}

#[cfg(test)]
mod tests {
    use tonic::metadata::MetadataValue;

    use super::*;

    fn make_request(auth_header: Option<&str>) -> Request<()> {
        let mut req = Request::new(());
        if let Some(val) = auth_header {
            req.metadata_mut()
                .insert("authorization", MetadataValue::try_from(val).unwrap());
        }
        req
    }

    #[test]
    fn valid_secret_is_accepted() {
        let interceptor = check_service_secret("my-secret".to_string());
        let req = make_request(Some("Bearer my-secret"));
        assert!(interceptor(req).is_ok());
    }

    #[test]
    fn wrong_secret_is_rejected() {
        let interceptor = check_service_secret("my-secret".to_string());
        let req = make_request(Some("Bearer wrong-secret"));
        let err = interceptor(req).unwrap_err();
        assert_eq!(err.code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn missing_header_is_rejected() {
        let interceptor = check_service_secret("my-secret".to_string());
        let req = make_request(None);
        let err = interceptor(req).unwrap_err();
        assert_eq!(err.code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn empty_header_is_rejected() {
        let interceptor = check_service_secret("my-secret".to_string());
        let req = make_request(Some(""));
        let err = interceptor(req).unwrap_err();
        assert_eq!(err.code(), tonic::Code::Unauthenticated);
    }
}
