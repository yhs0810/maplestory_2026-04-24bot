use axum::{
    extract::{Json, State},
    routing::post,
    Router,
};
use chrono::{TimeZone, Utc};
use serde::{Deserialize, Serialize};
use sqlx::mysql::{MySqlPool, MySqlPoolOptions};
use sqlx::Row;
use std::net::SocketAddr;

#[derive(Clone)]
struct AppState {
    db: MySqlPool,
}

#[derive(Deserialize)]
struct LoginRequest {
    user_id: String,
}

#[derive(Serialize)]
struct LoginResponse {
    status: String,
    expire_date: Option<String>,
    message: Option<String>,
}

#[derive(Deserialize)]
struct BasicRequest {
    user_id: String,
}

#[derive(Serialize)]
struct HeartbeatResponse {
    action: Option<String>,
}

#[tokio::main]
async fn main() {
    println!(">>> API Server starting...");

    // 데이터베이스 접속 정보 (서버 아이피, 관리자 root, 비밀번호, root 로컬 포트 3306)
    // 이 프로그램은 리눅스 서버 내부에서 실행되므로, 127.0.0.1 로컬호스트로 MySQL에 바로 접근하는 것이 가장 안전하고 빠릅니다.
    // 만약 사장님 윈도우 PC에서 테스트하신다면 127.0.0.1 부분을 93.127.129.57 로 바꿔서 실행하세요.
    let db_url = "mysql://user_account:Aa102331253910!@127.0.0.1:3306/maplestory_bot";

    let pool = MySqlPoolOptions::new()
        .max_connections(100) // 서버가 동시에 처리할 수 있는 DB 커넥션 최대 개수
        .connect(db_url)
        .await
        .expect("Failed to connect to MySQL! DB 서버가 켜져있는지 비밀번호가 맞는지 확인하세요.");

    let state = AppState { db: pool };

    let app = Router::new()
        .route("/api/login", post(login_handler))
        .route("/api/logout", post(logout_handler))
        .route("/api/heartbeat", post(heartbeat_handler))
        .with_state(state);

    // 0.0.0.0은 외부의 모든 웹 요청(유저 클라이언트 요청)을 허용한다는 뜻입니다.
    let idx = SocketAddr::from(([0, 0, 0, 0], 8080));
    let listener = tokio::net::TcpListener::bind(idx).await.unwrap();
    println!(
        "🚀 Server API is successfully running and listening on {}",
        idx
    );
    axum::serve(listener, app).await.unwrap();
}

/// 1. 로그인 요청 처리
async fn login_handler(
    State(state): State<AppState>,
    Json(payload): Json<LoginRequest>,
) -> Json<LoginResponse> {
    let user_id = payload.user_id;

    let user_res = sqlx::query("SELECT expire_date, is_login FROM users WHERE user_id = ?")
        .bind(&user_id)
        .fetch_optional(&state.db)
        .await;

    match user_res {
        Ok(Some(row)) => {
            let is_login: i8 = row.get("is_login");
            let expire_date: chrono::NaiveDateTime = row.get("expire_date");

            let expire_edmonton = chrono_tz::America::Edmonton
                .from_local_datetime(&expire_date)
                .single()
                .unwrap_or_else(|| chrono_tz::America::Edmonton.from_utc_datetime(&expire_date));

            // 에드먼턴 시간으로 현재 시간 구하기
            let current_time = Utc::now().with_timezone(&chrono_tz::America::Edmonton);

            if current_time >= expire_edmonton {
                return Json(LoginResponse {
                    status: "error".to_string(),
                    expire_date: None,
                    message: Some("만료된 계정입니다.".to_string()),
                });
            }

            // 중복 로그인 30초 핑 확인
            if is_login != 0 {
                let seconds_since_last_ping: Option<i64> = sqlx::query_scalar::<_, Option<i64>>(
                    "SELECT TIMESTAMPDIFF(SECOND, last_ping, NOW()) FROM users WHERE user_id = ?",
                )
                .bind(&user_id)
                .fetch_one(&state.db)
                .await
                .unwrap_or(Some(0));

                if let Some(diff) = seconds_since_last_ping {
                    if diff < 30 {
                        return Json(LoginResponse {
                            status: "error".to_string(),
                            expire_date: None,
                            message: Some("이미 로그인된 사용자입니다.".to_string()),
                        });
                    }
                }
            }

            // 모든 검증 통과 -> 로그인 상태 업데이트
            let _ =
                sqlx::query("UPDATE users SET is_login = 1, last_ping = NOW() WHERE user_id = ?")
                    .bind(&user_id)
                    .execute(&state.db)
                    .await;

            return Json(LoginResponse {
                status: "ok".to_string(),
                // 유저 클라이언트로 파싱하기 쉽게 국제 표준 시간(ISO) 문자열로 보냄
                expire_date: Some(expire_edmonton.with_timezone(&Utc).to_rfc3339()),
                message: None,
            });
        }
        Ok(None) => Json(LoginResponse {
            status: "error".to_string(),
            expire_date: None,
            message: Some("존재하지 않는 회원입니다.".to_string()),
        }),
        Err(e) => {
            println!("DB Error: {:?}", e);
            Json(LoginResponse {
                status: "error".to_string(),
                expire_date: None,
                message: Some("서버 내부 DB 접속 관련 에러 발생".to_string()),
            })
        }
    }
}

/// 2. 로그아웃 (프로그램 정상 종료 시)
async fn logout_handler(
    State(state): State<AppState>,
    Json(payload): Json<BasicRequest>,
) -> Json<serde_json::Value> {
    let _ = sqlx::query("UPDATE users SET is_login = 0 WHERE user_id = ?")
        .bind(payload.user_id)
        .execute(&state.db)
        .await;

    Json(serde_json::json!({"status": "ok"}))
}

/// 3. 지속적인 하트비트 생존 핑 및 만료시간 재검사
async fn heartbeat_handler(
    State(state): State<AppState>,
    Json(payload): Json<BasicRequest>,
) -> Json<HeartbeatResponse> {
    let user_id = payload.user_id;

    // 우선 핑을 업데이트합니다.
    let _ = sqlx::query("UPDATE users SET last_ping = NOW() WHERE user_id = ? AND is_login = 1")
        .bind(&user_id)
        .execute(&state.db)
        .await;

    // 그 다음 상태가 정상적인지 판별해 클라이언트에 명령을 내립니다.
    if let Ok(row) = sqlx::query("SELECT is_login, expire_date FROM users WHERE user_id = ?")
        .bind(&user_id)
        .fetch_one(&state.db)
        .await
    {
        let is_login: i8 = row.get("is_login");
        let expire_date: chrono::NaiveDateTime = row.get("expire_date");

        // 만약 관리자가 DB에서 is_login을 0으로 바꿔서 강제로 끊었다면 'kick' 액션을 반환
        if is_login == 0 {
            return Json(HeartbeatResponse {
                action: Some("kick".to_string()),
            });
        }

        let expire_edmonton = chrono_tz::America::Edmonton
            .from_local_datetime(&expire_date)
            .single()
            .unwrap_or_else(|| chrono_tz::America::Edmonton.from_utc_datetime(&expire_date));

        let current_time = Utc::now().with_timezone(&chrono_tz::America::Edmonton);

        // 사용 중간에 만료 기한이 지나버리면 DB도 끄고 킥 반환
        if current_time >= expire_edmonton {
            let _ = sqlx::query("UPDATE users SET is_login = 0 WHERE user_id = ?")
                .bind(&user_id)
                .execute(&state.db)
                .await;

            return Json(HeartbeatResponse {
                action: Some("kick".to_string()),
            });
        }
    }

    // 정상일 때는 아무 명령도 내리지 않음
    Json(HeartbeatResponse { action: None })
}
