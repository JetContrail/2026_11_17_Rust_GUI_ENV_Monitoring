//! WebAPI: authentication, RBAC, remote unlock, data access and HTTPS-only transport.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::NaiveDateTime;
use envmon_api::auth::TokenStore;
use envmon_api::{ApiState, DoorStatus, Role, router, server};
use envmon_core::demo::{self, ROOM_DOOR, ROOM_READER};
use envmon_core::door::{DoorState, LockState};
use envmon_core::model::User;
use envmon_core::system::{AccessPoint, ControlCommand, System};
use envmon_protocol::host::Clock;
use envmon_protocol::tls::{self, DevCa, TlsConnector};
use http_body_util::BodyExt;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;

#[derive(Debug)]
struct Fixed(NaiveDateTime, Instant);

impl Clock for Fixed {
    fn now(&self) -> (NaiveDateTime, Instant) {
        (self.0, self.1)
    }
}

fn app() -> (Router, Arc<Mutex<System>>) {
    let site = demo::site(100);
    let system = Arc::new(Mutex::new(
        System::new(site.directory, site.rules, site.doors).unwrap(),
    ));
    let mut tokens = TokenStore::new();
    tokens.add("viewer", Role::Viewer, "viewer-token");
    tokens.add("operator", Role::Operator, "operator-token");
    tokens.add("admin", Role::Admin, "admin-token");
    let clock = Arc::new(Fixed(
        demo::epoch() + chrono::TimeDelta::days(1),
        Instant::now(),
    ));
    (
        router(ApiState::new(Arc::clone(&system), tokens, clock)),
        system,
    )
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let req = match body {
        Some(b) => req
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn requests_without_a_valid_token_are_rejected() {
    let (app, _) = app();
    assert_eq!(
        call(&app, "GET", "/api/v1/health", None, None).await.0,
        StatusCode::OK
    );
    for token in [None, Some("wrong"), Some("")] {
        let (status, body) = call(&app, "GET", "/api/v1/doors", token, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(body["error"].is_string());
    }
    let (status, _) = call(&app, "POST", "/api/v1/doors/1/unlock", Some("bogus"), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn roles_limit_operations() {
    let (app, system) = app();
    assert_eq!(
        call(&app, "GET", "/api/v1/doors", Some("viewer-token"), None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/doors/1/unlock",
            Some("viewer-token"),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let user = serde_json::to_value(
        system
            .lock()
            .unwrap()
            .directory()
            .user(envmon_core::model::UserId(5))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        call(
            &app,
            "PUT",
            "/api/v1/users/5",
            Some("operator-token"),
            Some(user.clone())
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "PUT",
            "/api/v1/users/5",
            Some("admin-token"),
            Some(user)
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert!(system.lock().unwrap().history().next().is_none());
}

#[tokio::test]
async fn operator_unlocks_a_door_remotely() {
    let (app, system) = app();
    let door = ROOM_DOOR.0;
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/doors/{door}/unlock"),
        Some("operator-token"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["unlocked_by"], "operator");

    {
        let mut sys = system.lock().unwrap();
        assert_eq!(
            sys.door_indication(ROOM_DOOR).unwrap().lock,
            LockState::Unlocked
        );
        assert!(sys.take_commands().contains(&ControlCommand::Unlock {
            door: ROOM_DOOR,
            duration_ms: 5000
        }));
        let rec = sys.history().last().unwrap();
        assert_eq!(rec.point, AccessPoint::RemoteUnlock(ROOM_DOOR));
    }

    let (status, body) = call(&app, "GET", "/api/v1/doors", Some("viewer-token"), None).await;
    assert_eq!(status, StatusCode::OK);
    let doors: Vec<DoorStatus> = serde_json::from_value(body).unwrap();
    let room = doors.iter().find(|d| d.id == ROOM_DOOR).unwrap();
    assert_eq!(
        (room.indication.lock, room.indication.door),
        (LockState::Unlocked, DoorState::Closed)
    );

    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/doors/999/unlock",
            Some("operator-token"),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn users_history_alarms_and_objects_are_readable() {
    let (app, system) = app();
    {
        let mut sys = system.lock().unwrap();
        let at = demo::epoch() + chrono::TimeDelta::days(1);
        let now = Instant::now();
        sys.card_presented(ROOM_READER, demo::card(1), at, now)
            .unwrap();
        sys.door_sensor(ROOM_DOOR, true, at, now).unwrap();
        sys.tick(at, now + std::time::Duration::from_secs(11))
            .unwrap();
    }
    let (status, body) = call(&app, "GET", "/api/v1/users/1", Some("viewer-token"), None).await;
    assert_eq!(status, StatusCode::OK);
    let user: User = serde_json::from_value(body).unwrap();
    assert_eq!(user.cards, vec![demo::card(1)]);
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/users/999999",
            Some("viewer-token"),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );

    let (_, history) = call(
        &app,
        "GET",
        "/api/v1/history?limit=10",
        Some("viewer-token"),
        None,
    )
    .await;
    assert_eq!(history.as_array().unwrap().len(), 1);
    assert_eq!(history[0]["granted"], true);

    let (_, alarms) = call(
        &app,
        "GET",
        "/api/v1/alarms?active=true",
        Some("viewer-token"),
        None,
    )
    .await;
    assert_eq!(alarms[0]["kind"], "open_too_long");

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/doors/{}/acknowledge", ROOM_DOOR.0),
        Some("operator-token"),
        None,
    )
    .await;
    assert_eq!(
        (status, body["acknowledged"].clone()),
        (StatusCode::OK, Value::Bool(true))
    );

    let (status, objects) = call(&app, "GET", "/api/v1/objects", Some("viewer-token"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(objects["version"].as_u64().unwrap() > 0);
    let v = objects["version"].as_u64().unwrap();
    let (_, unchanged) = call(
        &app,
        "GET",
        &format!("/api/v1/objects?since={v}"),
        Some("viewer-token"),
        None,
    )
    .await;
    assert!(unchanged["objects"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn served_over_tls13_only() {
    let (app, _) = app();
    let ca = DevCa::generate("api test CA").unwrap();
    let config = tls::https_server_config(&ca.issue_server(&["localhost"]).unwrap()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(server::serve_https(listener, config, app));
    let request = "GET /api/v1/doors HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer viewer-token\r\nConnection: close\r\n\r\n";

    let connector = TlsConnector::from(tls::client_config(&[ca.certificate()], None).unwrap());
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut stream = connector
        .connect(tls::server_name("localhost").unwrap(), tcp)
        .await
        .unwrap();
    assert_eq!(
        stream.get_ref().1.protocol_version(),
        Some(tokio_rustls::rustls::ProtocolVersion::TLSv1_3)
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("\"name\""));

    // Plain HTTP on the same port gets no HTTP response.
    let mut plain = tokio::net::TcpStream::connect(addr).await.unwrap();
    plain.write_all(request.as_bytes()).await.unwrap();
    let mut bytes = Vec::new();
    let _ = plain.read_to_end(&mut bytes).await;
    assert!(!bytes.starts_with(b"HTTP/"));
}
