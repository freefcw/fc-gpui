mod async_body;
pub mod github;
pub mod github_download;

pub use anyhow::{Result, anyhow};
pub use async_body::{AsyncBody, Inner};
use derive_more::Deref;
use http::HeaderValue;
pub use http::{self, Method, Request, Response, StatusCode, Uri};

use futures::future::BoxFuture;
use http::request::Builder;
use parking_lot::Mutex;
#[cfg(feature = "test-support")]
use std::fmt;
use std::{any::type_name, sync::Arc, time::Duration};
pub use url::Url;

#[derive(Default, Debug, Clone, PartialEq, Eq, Hash)]
pub enum RedirectPolicy {
    #[default]
    NoFollow,
    FollowLimit(u32),
    FollowAll,
}

/// Legacy boolean redirect policy retained for source compatibility.
///
/// Prefer [`RedirectPolicy`], which can express bounded redirect following.
#[deprecated(note = "use RedirectPolicy instead")]
pub struct FollowRedirects(pub bool);

#[allow(deprecated)]
impl From<FollowRedirects> for RedirectPolicy {
    fn from(value: FollowRedirects) -> Self {
        if value.0 {
            Self::FollowAll
        } else {
            Self::NoFollow
        }
    }
}

/// The caller's deadline for a complete HTTP request, including its response body.
///
/// This value is stored in [`http::Request::extensions`]. Every concrete [`HttpClient`]
/// adapter is responsible for reading and enforcing it; the trait cannot enforce a
/// deadline on an arbitrary implementation by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestTimeout(pub Duration);

impl RequestTimeout {
    /// Reads the request deadline attached by [`HttpRequestExt::timeout`].
    pub fn from_request<T>(request: &http::Request<T>) -> Option<Duration> {
        request.extensions().get::<Self>().map(|timeout| timeout.0)
    }
}

pub trait HttpRequestExt {
    /// Conditionally modify self with the given closure.
    fn when(self, condition: bool, then: impl FnOnce(Self) -> Self) -> Self
    where
        Self: Sized,
    {
        if condition { then(self) } else { self }
    }

    /// Conditionally unwrap and modify self with the given closure, if the given option is Some.
    fn when_some<T>(self, option: Option<T>, then: impl FnOnce(Self, T) -> Self) -> Self
    where
        Self: Sized,
    {
        match option {
            Some(value) => then(self, value),
            None => self,
        }
    }

    /// Whether or not to follow redirects
    fn follow_redirects(self, follow: RedirectPolicy) -> Self;

    /// Requests a deadline for the complete HTTP request, including its response body.
    ///
    /// The deadline is attached as [`RequestTimeout`] metadata. Concrete HTTP client
    /// adapters must read this metadata and enforce the deadline around their full
    /// request/response operation.
    fn timeout(self, timeout: Duration) -> Self;
}

impl HttpRequestExt for http::request::Builder {
    fn follow_redirects(self, follow: RedirectPolicy) -> Self {
        self.extension(follow)
    }

    fn timeout(self, timeout: Duration) -> Self {
        debug_assert!(!timeout.is_zero(), "timeout must be positive");
        self.extension(RequestTimeout(timeout))
    }
}

pub trait HttpClient: 'static + Send + Sync {
    fn type_name(&self) -> &'static str;

    fn user_agent(&self) -> Option<&HeaderValue>;

    fn send(
        &self,
        req: http::Request<AsyncBody>,
    ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>>;

    fn get(
        &self,
        uri: &str,
        body: AsyncBody,
        follow_redirects: bool,
    ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
        let request = Builder::new()
            .uri(uri)
            .follow_redirects(if follow_redirects {
                RedirectPolicy::FollowAll
            } else {
                RedirectPolicy::NoFollow
            })
            .body(body);

        match request {
            Ok(request) => self.send(request),
            Err(e) => Box::pin(async move { Err(e.into()) }),
        }
    }

    fn post_json(
        &self,
        uri: &str,
        body: AsyncBody,
    ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
        let request = Builder::new()
            .uri(uri)
            .method(Method::POST)
            .header("Content-Type", "application/json")
            .body(body);

        match request {
            Ok(request) => self.send(request),
            Err(e) => Box::pin(async move { Err(e.into()) }),
        }
    }

    fn proxy(&self) -> Option<&Url>;

    #[cfg(feature = "test-support")]
    fn as_fake(&self) -> &FakeHttpClient {
        panic!("called as_fake on {}", type_name::<Self>())
    }

    /// Sends a multipart form through the `reqwest` compatibility API.
    ///
    /// Requires the `reqwest` feature. The default implementation reports that
    /// multipart sending is unsupported by returning `"not implemented"`.
    /// For protocol-level clients, encode the multipart body and headers in an
    /// `http::Request<AsyncBody>` and use [`HttpClient::send`] instead.
    #[cfg(feature = "reqwest")]
    fn send_multipart_form<'a>(
        &'a self,
        _url: &str,
        _request: reqwest::multipart::Form,
    ) -> BoxFuture<'a, anyhow::Result<Response<AsyncBody>>> {
        Box::pin(async move { Err(anyhow!("not implemented")) })
    }
}

/// An [`HttpClient`] that may have a proxy.
#[derive(Deref)]
pub struct HttpClientWithProxy {
    #[deref]
    client: Arc<dyn HttpClient>,
    proxy: Option<Url>,
}

impl HttpClientWithProxy {
    /// Returns a new [`HttpClientWithProxy`] with the given proxy URL.
    pub fn new(client: Arc<dyn HttpClient>, proxy_url: Option<String>) -> Self {
        let proxy_url = proxy_url
            .and_then(|proxy| proxy.parse().ok())
            .or_else(read_proxy_from_env);

        Self::new_url(client, proxy_url)
    }
    pub fn new_url(client: Arc<dyn HttpClient>, proxy_url: Option<Url>) -> Self {
        Self {
            client,
            proxy: proxy_url,
        }
    }
}

impl HttpClient for HttpClientWithProxy {
    fn send(
        &self,
        req: Request<AsyncBody>,
    ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
        self.client.send(req)
    }

    fn user_agent(&self) -> Option<&HeaderValue> {
        self.client.user_agent()
    }

    fn proxy(&self) -> Option<&Url> {
        self.proxy.as_ref()
    }

    fn type_name(&self) -> &'static str {
        self.client.type_name()
    }

    #[cfg(feature = "test-support")]
    fn as_fake(&self) -> &FakeHttpClient {
        self.client.as_fake()
    }

    #[cfg(feature = "reqwest")]
    fn send_multipart_form<'a>(
        &'a self,
        url: &str,
        form: reqwest::multipart::Form,
    ) -> BoxFuture<'a, anyhow::Result<Response<AsyncBody>>> {
        self.client.send_multipart_form(url, form)
    }
}

/// An [`HttpClient`] that has a base URL.
pub struct HttpClientWithUrl {
    base_url: Mutex<String>,
    client: HttpClientWithProxy,
}

impl std::ops::Deref for HttpClientWithUrl {
    type Target = HttpClientWithProxy;

    fn deref(&self) -> &Self::Target {
        &self.client
    }
}

impl HttpClientWithUrl {
    /// Returns a new [`HttpClientWithUrl`] with the given base URL.
    pub fn new(
        client: Arc<dyn HttpClient>,
        base_url: impl Into<String>,
        proxy_url: Option<String>,
    ) -> Self {
        let client = HttpClientWithProxy::new(client, proxy_url);

        Self {
            base_url: Mutex::new(base_url.into()),
            client,
        }
    }

    pub fn new_url(
        client: Arc<dyn HttpClient>,
        base_url: impl Into<String>,
        proxy_url: Option<Url>,
    ) -> Self {
        let client = HttpClientWithProxy::new_url(client, proxy_url);

        Self {
            base_url: Mutex::new(base_url.into()),
            client,
        }
    }

    /// Returns the base URL.
    pub fn base_url(&self) -> String {
        self.base_url.lock().clone()
    }

    /// Sets the base URL.
    pub fn set_base_url(&self, base_url: impl Into<String>) {
        let base_url = base_url.into();
        *self.base_url.lock() = base_url;
    }

    /// Builds a URL using the given path.
    pub fn build_url(&self, path: &str) -> String {
        format!("{}{}", self.base_url(), path)
    }

    /// Builds a Zed API URL using the given path.
    #[deprecated(
        note = "Zed-specific endpoint routing; configure API endpoints and construct URLs in application code"
    )]
    pub fn build_zed_api_url(&self, path: &str, query: &[(&str, &str)]) -> Result<Url> {
        let base_url = self.base_url();
        let base_api_url = match base_url.as_ref() {
            "https://zed.dev" => "https://api.zed.dev",
            "https://staging.zed.dev" => "https://api-staging.zed.dev",
            "http://localhost:3000" => "http://localhost:8080",
            other => other,
        };

        Ok(Url::parse_with_params(
            &format!("{}{}", base_api_url, path),
            query,
        )?)
    }

    /// Builds a Zed Cloud URL using the given path.
    #[deprecated(
        note = "Zed-specific endpoint routing; configure cloud endpoints and construct URLs in application code"
    )]
    pub fn build_zed_cloud_url(&self, path: &str, query: &[(&str, &str)]) -> Result<Url> {
        let base_url = self.base_url();
        let base_api_url = match base_url.as_ref() {
            "https://zed.dev" => "https://cloud.zed.dev",
            "https://staging.zed.dev" => "https://cloud.zed.dev",
            "http://localhost:3000" => "http://localhost:8787",
            other => other,
        };

        Ok(Url::parse_with_params(
            &format!("{}{}", base_api_url, path),
            query,
        )?)
    }

    /// Builds a Zed LLM URL using the given path.
    #[deprecated(
        note = "Zed-specific endpoint routing; configure LLM endpoints and construct URLs in application code"
    )]
    pub fn build_zed_llm_url(&self, path: &str, query: &[(&str, &str)]) -> Result<Url> {
        let base_url = self.base_url();
        let base_api_url = match base_url.as_ref() {
            "https://zed.dev" => "https://cloud.zed.dev",
            "https://staging.zed.dev" => "https://llm-staging.zed.dev",
            "http://localhost:3000" => "http://localhost:8787",
            other => other,
        };

        Ok(Url::parse_with_params(
            &format!("{}{}", base_api_url, path),
            query,
        )?)
    }
}

impl HttpClient for HttpClientWithUrl {
    fn send(
        &self,
        req: Request<AsyncBody>,
    ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
        self.client.send(req)
    }

    fn user_agent(&self) -> Option<&HeaderValue> {
        self.client.user_agent()
    }

    fn proxy(&self) -> Option<&Url> {
        self.client.proxy.as_ref()
    }

    fn type_name(&self) -> &'static str {
        self.client.type_name()
    }

    #[cfg(feature = "test-support")]
    fn as_fake(&self) -> &FakeHttpClient {
        self.client.as_fake()
    }

    #[cfg(feature = "reqwest")]
    fn send_multipart_form<'a>(
        &'a self,
        url: &str,
        request: reqwest::multipart::Form,
    ) -> BoxFuture<'a, anyhow::Result<Response<AsyncBody>>> {
        self.client.send_multipart_form(url, request)
    }
}

pub fn read_proxy_from_env() -> Option<Url> {
    const ENV_VARS: &[&str] = &[
        "ALL_PROXY",
        "all_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ];

    ENV_VARS
        .iter()
        .find_map(|var| std::env::var(var).ok())
        .and_then(|env| env.parse().ok())
}

pub fn read_no_proxy_from_env() -> Option<String> {
    const ENV_VARS: &[&str] = &["NO_PROXY", "no_proxy"];

    ENV_VARS.iter().find_map(|var| std::env::var(var).ok())
}

pub struct BlockedHttpClient;

impl BlockedHttpClient {
    pub fn new() -> Self {
        BlockedHttpClient
    }
}

impl HttpClient for BlockedHttpClient {
    fn send(
        &self,
        _req: Request<AsyncBody>,
    ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
        Box::pin(async {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "BlockedHttpClient disallowed request",
            )
            .into())
        })
    }

    fn user_agent(&self) -> Option<&HeaderValue> {
        None
    }

    fn proxy(&self) -> Option<&Url> {
        None
    }

    fn type_name(&self) -> &'static str {
        type_name::<Self>()
    }

    #[cfg(feature = "test-support")]
    fn as_fake(&self) -> &FakeHttpClient {
        panic!("called as_fake on {}", type_name::<Self>())
    }
}

#[cfg(feature = "test-support")]
type FakeHttpHandler = Arc<
    dyn Fn(Request<AsyncBody>) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>>
        + Send
        + Sync
        + 'static,
>;

#[cfg(feature = "test-support")]
pub struct FakeHttpClient {
    handler: Mutex<Option<FakeHttpHandler>>,
    user_agent: HeaderValue,
}

#[cfg(feature = "test-support")]
impl FakeHttpClient {
    pub fn create<Fut, F>(handler: F) -> Arc<HttpClientWithUrl>
    where
        Fut: futures::Future<Output = anyhow::Result<Response<AsyncBody>>> + Send + 'static,
        F: Fn(Request<AsyncBody>) -> Fut + Send + Sync + 'static,
    {
        Arc::new(HttpClientWithUrl {
            base_url: Mutex::new("http://test.example".into()),
            client: HttpClientWithProxy {
                client: Arc::new(Self {
                    handler: Mutex::new(Some(Arc::new(move |req| Box::pin(handler(req))))),
                    user_agent: HeaderValue::from_static(type_name::<Self>()),
                }),
                proxy: None,
            },
        })
    }

    pub fn with_404_response() -> Arc<HttpClientWithUrl> {
        Self::create(|_| async move {
            Ok(Response::builder()
                .status(404)
                .body(Default::default())
                .unwrap())
        })
    }

    pub fn with_200_response() -> Arc<HttpClientWithUrl> {
        Self::create(|_| async move {
            Ok(Response::builder()
                .status(200)
                .body(Default::default())
                .unwrap())
        })
    }

    pub fn replace_handler<Fut, F>(&self, new_handler: F)
    where
        Fut: futures::Future<Output = anyhow::Result<Response<AsyncBody>>> + Send + 'static,
        F: Fn(FakeHttpHandler, Request<AsyncBody>) -> Fut + Send + Sync + 'static,
    {
        let mut handler = self.handler.lock();
        let old_handler = handler.take().unwrap();
        *handler = Some(Arc::new(move |req| {
            Box::pin(new_handler(old_handler.clone(), req))
        }));
    }
}

#[cfg(feature = "test-support")]
impl fmt::Debug for FakeHttpClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FakeHttpClient").finish()
    }
}

#[cfg(feature = "test-support")]
impl HttpClient for FakeHttpClient {
    fn send(
        &self,
        req: Request<AsyncBody>,
    ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
        ((self.handler.lock().as_ref().unwrap())(req)) as _
    }

    fn user_agent(&self) -> Option<&HeaderValue> {
        Some(&self.user_agent)
    }

    fn proxy(&self) -> Option<&Url> {
        None
    }

    fn type_name(&self) -> &'static str {
        type_name::<Self>()
    }

    fn as_fake(&self) -> &FakeHttpClient {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AsyncBody, HttpClient, HttpClientWithProxy, HttpClientWithUrl, HttpRequestExt,
        RedirectPolicy, Request, RequestTimeout, Response, Url,
    };
    use futures::{AsyncReadExt, future::BoxFuture};
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };

    struct RecordingClient {
        requests: Arc<Mutex<Vec<Request<AsyncBody>>>>,
        user_agent: http::HeaderValue,
    }

    impl HttpClient for RecordingClient {
        fn type_name(&self) -> &'static str {
            std::any::type_name::<Self>()
        }

        fn user_agent(&self) -> Option<&http::HeaderValue> {
            Some(&self.user_agent)
        }

        fn send(
            &self,
            request: Request<AsyncBody>,
        ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
            self.requests.lock().unwrap().push(request);
            Box::pin(async { Ok(Response::new("response".into())) })
        }

        fn proxy(&self) -> Option<&Url> {
            None
        }

        #[cfg(feature = "reqwest")]
        fn send_multipart_form<'a>(
            &'a self,
            url: &str,
            _: reqwest::multipart::Form,
        ) -> BoxFuture<'a, anyhow::Result<Response<AsyncBody>>> {
            self.send(
                Request::builder()
                    .uri(url)
                    .body(AsyncBody::empty())
                    .unwrap(),
            )
        }
    }

    fn recording_clients() -> (
        Vec<Arc<dyn HttpClient>>,
        Arc<Mutex<Vec<Request<AsyncBody>>>>,
    ) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let backend = Arc::new(RecordingClient {
            requests: requests.clone(),
            user_agent: http::HeaderValue::from_static("contract-test"),
        });
        let proxy = Url::parse("http://proxy.example:8080").unwrap();
        let clients: Vec<Arc<dyn HttpClient>> = vec![
            backend.clone(),
            Arc::new(HttpClientWithProxy::new_url(
                backend.clone(),
                Some(proxy.clone()),
            )),
            Arc::new(HttpClientWithUrl::new_url(
                backend,
                "https://example.com",
                Some(proxy),
            )),
        ];
        (clients, requests)
    }

    #[test]
    fn protocol_clients_preserve_request_metadata_and_bodies() {
        let (clients, requests) = recording_clients();
        for client in clients {
            let request = Request::builder()
                .method(http::Method::PUT)
                .uri("https://example.com/resource")
                .header("x-contract", "preserved")
                .follow_redirects(RedirectPolicy::FollowLimit(2))
                .timeout(Duration::from_secs(3))
                .body(AsyncBody::from("request"))
                .unwrap();
            let mut response = futures::executor::block_on(client.send(request)).unwrap();
            let mut body = String::new();
            futures::executor::block_on(response.body_mut().read_to_string(&mut body)).unwrap();
            assert_eq!(body, "response");
            assert_eq!(client.user_agent().unwrap(), "contract-test");

            let mut request = requests.lock().unwrap().pop().unwrap();
            assert_eq!(request.method(), http::Method::PUT);
            assert_eq!(request.uri(), "https://example.com/resource");
            assert_eq!(request.headers()["x-contract"], "preserved");
            assert_eq!(
                request.extensions().get::<RedirectPolicy>(),
                Some(&RedirectPolicy::FollowLimit(2))
            );
            assert_eq!(
                RequestTimeout::from_request(&request),
                Some(Duration::from_secs(3))
            );
            body.clear();
            futures::executor::block_on(request.body_mut().read_to_string(&mut body)).unwrap();
            assert_eq!(body, "request");
        }
    }

    #[test]
    fn protocol_clients_support_get_and_json_without_backend_types() {
        let (clients, requests) = recording_clients();
        for client in clients {
            futures::executor::block_on(client.get(
                "https://example.com",
                AsyncBody::empty(),
                true,
            ))
            .unwrap();
            let request = requests.lock().unwrap().pop().unwrap();
            assert_eq!(request.method(), http::Method::GET);
            assert_eq!(
                request.extensions().get::<RedirectPolicy>(),
                Some(&RedirectPolicy::FollowAll)
            );

            futures::executor::block_on(client.post_json("https://example.com", "{}".into()))
                .unwrap();
            let request = requests.lock().unwrap().pop().unwrap();
            assert_eq!(request.method(), http::Method::POST);
            assert_eq!(
                request.headers()[http::header::CONTENT_TYPE],
                "application/json"
            );
        }
    }

    #[cfg(feature = "reqwest")]
    #[test]
    fn reqwest_multipart_api_is_forwarded_by_wrappers() {
        let (clients, requests) = recording_clients();
        for client in clients {
            futures::executor::block_on(client.send_multipart_form(
                "https://example.com/upload",
                reqwest::multipart::Form::new().text("name", "value"),
            ))
            .unwrap();
            let request = requests.lock().unwrap().pop().unwrap();
            assert_eq!(request.uri(), "https://example.com/upload");
        }
    }

    #[test]
    #[allow(deprecated)]
    fn legacy_redirect_policy_converts_both_boolean_values() {
        use super::{FollowRedirects, RedirectPolicy};

        assert_eq!(
            RedirectPolicy::from(FollowRedirects(false)),
            RedirectPolicy::NoFollow
        );
        assert_eq!(
            RedirectPolicy::from(FollowRedirects(true)),
            RedirectPolicy::FollowAll
        );
    }

    #[test]
    fn request_timeout_is_extractable_from_request() {
        let request = http::Request::builder()
            .uri("https://example.com")
            .timeout(Duration::from_secs(3))
            .body(())
            .unwrap();

        assert_eq!(
            RequestTimeout::from_request(&request),
            Some(Duration::from_secs(3))
        );
    }

    #[test]
    fn request_timeout_is_absent_when_not_configured() {
        let request = http::Request::builder()
            .uri("https://example.com")
            .body(())
            .unwrap();

        assert_eq!(RequestTimeout::from_request(&request), None);
    }
}
