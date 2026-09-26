use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use serde_json::{Value, json};
use tower::ServiceExt as _;

use super::{AdminTestFixture, PRIMARY_GROUP_ID};

async fn request(
    fixture: &AdminTestFixture,
    path: &str,
    body: Option<Value>,
    session: bool,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .header("x-request-id", "req_public_import_tests")
        .header(header::CONTENT_TYPE, "application/json")
        .uri(path)
        .method(if body.is_some() { "POST" } else { "GET" });
    if session {
        builder = builder.header(header::COOKIE, "cpr_session=valid-session");
    }
    if let Some(token) = token {
        builder = builder.header("x-import-token", token);
    }
    let body = body.map_or_else(Body::empty, |body| Body::from(body.to_string()));
    let response = crate::openai::api_router_with_admin(fixture.services.clone())
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn admin_configuration_should_require_administrator_authentication() {
    let fixture = AdminTestFixture::new().await;
    for (path, body) in [
        ("/api/admin/public-import", None),
        (
            "/api/admin/public-import/create",
            Some(json!({"name": "迷茫", "enabled": false, "groupIds": [], "pinTurnState": true, "expiresAt": null})),
        ),
        (
            "/api/admin/public-import/update",
            Some(json!({"id": "x", "name": "迷茫", "enabled": false, "groupIds": [], "pinTurnState": true, "expiresAt": null})),
        ),
        ("/api/admin/public-import/delete", Some(json!({"id": "x"}))),
        ("/api/admin/public-import/rotate-token", Some(json!({"id": "x"}))),
    ] {
        let (status, _) = request(&fixture, path, body, false, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
    }
}

#[tokio::test]
async fn public_entry_should_work_without_session_only_with_the_link_token() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");

    // 初始没有任何号商配置。
    let (status, list) = request(&fixture, "/api/admin/public-import", None, true, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["data"]["configs"], json!([]));

    // 有效期字段必填：漏传不能被当成「长期有效」。
    let (status, _) = request(
        &fixture,
        "/api/admin/public-import/create",
        Some(json!({"name": "迷茫", "enabled": true, "groupIds": [PRIMARY_GROUP_ID], "pinTurnState": true})),
        true,
        None,
    )
    .await;
    assert!(status.is_client_error());

    // 新建一个号商配置。
    let (status, created) = request(
        &fixture,
        "/api/admin/public-import/create",
        Some(json!({"name": "迷茫", "enabled": true, "groupIds": [PRIMARY_GROUP_ID], "pinTurnState": true, "expiresAt": "2999-01-01T00:00:00Z"})),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(created["data"]["name"], "迷茫");
    assert_eq!(created["data"]["enabled"], true);
    assert_eq!(created["data"]["expiresAt"], "2999-01-01T00:00:00Z");
    let token = created["data"]["token"].as_str().expect("token").to_owned();
    let id = created["data"]["id"].as_str().expect("id").to_owned();

    // 缺令牌和错令牌对外表现与未开启一致。
    for candidate in [None, Some("imp-wrong")] {
        let (status, _) =
            request(&fixture, "/api/public-import/entry", None, false, candidate).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    let (status, entry) = request(
        &fixture,
        "/api/public-import/entry",
        None,
        false,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(entry["data"]["groupNames"], json!(["Alpha routing"]));
    assert_eq!(entry["data"]["pinTurnState"], true);
    assert_eq!(entry["data"]["expiresAt"], "2999-01-01T00:00:00Z");
    assert!(entry["data"].get("groupIds").is_none());

    let document =
        json!({"data": {"accounts": [{"name": "a", "credentials": {"refresh_token": "rt"}}]}});
    let (status, _) = request(
        &fixture,
        "/api/public-import/accounts",
        Some(document.clone()),
        false,
        Some("imp-wrong"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // 夹具里没有通过测试的代理：请求已越过令牌校验并在随机出口处失败关闭。
    let (status, body) = request(
        &fixture,
        "/api/public-import/accounts",
        Some(document),
        false,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["message"].as_str().expect("message").contains("代理"));

    // 轮换令牌后旧令牌立即失效。
    let (status, rotated) = request(
        &fixture,
        "/api/admin/public-import/rotate-token",
        Some(json!({"id": id})),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(rotated["data"]["token"], token);
    let (status, _) = request(
        &fixture,
        "/api/public-import/entry",
        None,
        false,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 删除后列表清空。
    let (status, _) = request(
        &fixture,
        "/api/admin/public-import/delete",
        Some(json!({"id": id})),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, list) = request(&fixture, "/api/admin/public-import", None, true, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["data"]["configs"], json!([]));
}
