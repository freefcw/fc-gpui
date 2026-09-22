use std::sync::Arc;

use futures::{AsyncReadExt as _, FutureExt as _};
use http_client::{
    AsyncBody, HttpClient, RequestTimeout, Response, Url,
    http::{HeaderValue, Request},
};

pub struct ReqwestExampleClient {
    client: reqwest::Client,
    user_agent: HeaderValue,
}

impl ReqwestExampleClient {
    pub fn user_agent(user_agent: &'static str) -> anyhow::Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder().user_agent(user_agent).build()?,
            user_agent: HeaderValue::from_static(user_agent),
        })
    }
}

impl HttpClient for ReqwestExampleClient {
    fn type_name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    fn user_agent(&self) -> Option<&HeaderValue> {
        Some(&self.user_agent)
    }

    fn send(
        &self,
        req: Request<AsyncBody>,
    ) -> futures::future::BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
        let client = self.client.clone();
        async move {
            let (parts, mut body) = req.into_parts();
            let timeout = parts.extensions.get::<RequestTimeout>().copied();
            let mut request_body = Vec::new();
            body.read_to_end(&mut request_body).await?;

            let mut request = client.request(parts.method, parts.uri.to_string());
            request = request.headers(parts.headers);
            if let Some(timeout) = timeout {
                request = request.timeout(timeout.0);
            }
            let response = request.body(request_body).send().await?;
            let mut builder = Response::builder().status(response.status());

            if let Some(headers) = builder.headers_mut() {
                headers.extend(response.headers().clone());
            }

            let body = response.bytes().await?.to_vec();
            Ok(builder.body(AsyncBody::from(body))?)
        }
        .boxed()
    }

    fn proxy(&self) -> Option<&Url> {
        None
    }
}

pub fn new_http_client() -> anyhow::Result<Arc<dyn HttpClient>> {
    Ok(Arc::new(ReqwestExampleClient::user_agent("gpui example")?))
}
