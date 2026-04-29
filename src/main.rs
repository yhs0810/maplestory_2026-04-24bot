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
    user_db: MySqlPool,   // For login, logout, heartbeat
    seller_db: MySqlPool, // For adding/updating users
}

#[derive(Deserialize)]
struct LoginRequest {
    user_id: String,
    program: Option<String>,
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
    program: Option<String>,
}

#[derive(Deserialize)]
struct AddUserRequest {
    user_id: String,
    days: u32,
    who_added: String,
    discord_tele_id: String,
}

#[derive(Deserialize)]
struct UpdateAutoLieRequest {
    user_id: String,
    enable: i8,
}

#[derive(Serialize)]
struct AddUserResponse {
    status: String,
    message: String,
}

#[derive(Serialize)]
struct HeartbeatResponse {
    action: Option<String>,
}

#[tokio::main]
async fn main() {
    println!(">>> API Server starting...");

    // 데이터베이스 접속 정보
    let user_db_url = "mysql://user_account:Aa102331253910!@127.0.0.1:3306/maplestory_bot";
    let user_pool = MySqlPoolOptions::new()
        .max_connections(50)
        .connect(user_db_url)
        .await
        .expect("Failed to connect to User DB!");

    let seller_db_url = "mysql://seller:a10233@127.0.0.1:3306/maplestory_bot";
    let seller_pool = MySqlPoolOptions::new()
        .max_connections(10)
        .connect(seller_db_url)
        .await
        .expect("Failed to connect to Seller DB!");

    // 데이터베이스 컬럼 자동 추가 (에러는 무시)
    let _ = sqlx::query(
        "ALTER TABLE users ADD COLUMN auto_lie_last_ping TIMESTAMP DEFAULT CURRENT_TIMESTAMP",
    )
    .execute(&user_pool)
    .await;

    let state = AppState {
        user_db: user_pool,
        seller_db: seller_pool,
    };

    let app = Router::new()
        .route("/api/login", post(login_handler))
        .route("/api/logout", post(logout_handler))
        .route("/api/heartbeat", post(heartbeat_handler))
        .route("/api/add_user", post(add_user_handler))
        .route("/api/update_auto_lie", post(update_auto_lie_handler))
        .with_state(state.clone());

    let idx = SocketAddr::from(([0, 0, 0, 0], 8080));
    let listener = tokio::net::TcpListener::bind(idx).await.unwrap();
    println!(
        "🚀 Server API is successfully running and listening on {}",
        idx
    );

    // [자동 로그아웃] 60초 이상 핑 없는 경우 세션 종료
    let cleaner_pool = state.user_db.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            // auto_lie_login 정리 (프로그램1)
            let _ = sqlx::query("UPDATE users SET auto_lie_login = 0 WHERE auto_lie_login = 1 AND TIMESTAMPDIFF(SECOND, auto_lie_last_ping, NOW()) > 60")
                .execute(&cleaner_pool).await;

            // is_login 정리 (프로그램2)
            let _ = sqlx::query("UPDATE users SET is_login = 0 WHERE is_login = 1 AND TIMESTAMPDIFF(SECOND, last_ping, NOW()) > 60")
                .execute(&cleaner_pool).await;
        }
    });

    axum::serve(listener, app).await.unwrap();
}

/// 1. 로그인 요청 처리
async fn login_handler(
    State(state): State<AppState>,
    Json(payload): Json<LoginRequest>,
) -> Json<LoginResponse> {
    let user_id = payload.user_id;

    println!(
        ">>> Login request from: {}, program: {:?}",
        user_id, payload.program
    );

    let user_res = sqlx::query(
        "SELECT expire_date, auto_lie, auto_lie_login, is_login FROM users WHERE user_id = ?",
    )
    .bind(&user_id)
    .fetch_optional(&state.user_db)
    .await;

    match user_res {
        Ok(Some(row)) => {
            let expire_date: chrono::NaiveDateTime = row.get("expire_date");
            let auto_lie: i8 = row.try_get("auto_lie").unwrap_or(0);
            let auto_lie_login: i8 = row.try_get("auto_lie_login").unwrap_or(0);
            let is_login: i8 = row.try_get("is_login").unwrap_or(0);

            // 공통: 만료일 체크
            let expire_edmonton = chrono_tz::America::Edmonton
                .from_local_datetime(&expire_date)
                .single()
                .unwrap_or_else(|| chrono_tz::America::Edmonton.from_utc_datetime(&expire_date));

            let current_time = Utc::now().with_timezone(&chrono_tz::America::Edmonton);
            if current_time >= expire_edmonton {
                return Json(LoginResponse {
                    status: "error".to_string(),
                    expire_date: None,
                    message: Some("만료된 계정입니다.".to_string()),
                });
            }

            // 프로그램 종류에 따른 분기 (프로그램1: auto_lie, 프로그램2: 일반(bot-server))
            if payload.program.as_deref() == Some("auto_lie") {
                // 프로그램 1 (오토거탐)
                if auto_lie != 1 {
                    return Json(LoginResponse {
                        status: "error".to_string(),
                        expire_date: None,
                        message: Some(format!("사용 권한이 없습니다 (auto_lie: {}).", auto_lie)),
                    });
                }

                if auto_lie_login != 0 {
                    let diff: i64 = sqlx::query_scalar(
                        "SELECT TIMESTAMPDIFF(SECOND, auto_lie_last_ping, NOW()) FROM users WHERE user_id = ?",
                    )
                    .bind(&user_id)
                    .fetch_one(&state.user_db)
                    .await
                    .unwrap_or(999);
                    if diff < 30 {
                        return Json(LoginResponse {
                            status: "error".to_string(),
                            expire_date: None,
                            message: Some("이미 다른 기기에서 사용 중입니다.".to_string()),
                        });
                    }
                }

                let _ = sqlx::query(
                    "UPDATE users SET auto_lie_login = 1, auto_lie_last_ping = NOW() WHERE user_id = ?",
                )
                .bind(&user_id)
                .execute(&state.user_db)
                .await;
            } else {
                // 프로그램 2 (기본 봇 서버)
                if is_login != 0 {
                    let diff: i64 = sqlx::query_scalar(
                        "SELECT TIMESTAMPDIFF(SECOND, last_ping, NOW()) FROM users WHERE user_id = ?",
                    )
                    .bind(&user_id)
                    .fetch_one(&state.user_db)
                    .await
                    .unwrap_or(999);
                    if diff < 30 {
                        return Json(LoginResponse {
                            status: "error".to_string(),
                            expire_date: None,
                            message: Some("이미 다른 기기에서 사용 중입니다.".to_string()),
                        });
                    }
                }

                let _ = sqlx::query(
                    "UPDATE users SET is_login = 1, last_ping = NOW() WHERE user_id = ?",
                )
                .bind(&user_id)
                .execute(&state.user_db)
                .await;
            }

            return Json(LoginResponse {
                status: "ok".to_string(),
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
            println!("Login Error: {:?}", e);
            Json(LoginResponse {
                status: "error".to_string(),
                expire_date: None,
                message: Some("서버 내부 DB 에러 발생".to_string()),
            })
        }
    }
}

/// 2. 로그아웃 요청 처리
async fn logout_handler(
    State(state): State<AppState>,
    Json(payload): Json<BasicRequest>,
) -> Json<serde_json::Value> {
    if payload.program.as_deref() == Some("auto_lie") {
        let _ = sqlx::query("UPDATE users SET auto_lie_login = 0 WHERE user_id = ?")
            .bind(payload.user_id)
            .execute(&state.user_db)
            .await;
    } else {
        let _ = sqlx::query("UPDATE users SET is_login = 0 WHERE user_id = ?")
            .bind(payload.user_id)
            .execute(&state.user_db)
            .await;
    }
    Json(serde_json::json!({"status": "ok"}))
}

/// 3. 하트비트 요청 처리
async fn heartbeat_handler(
    State(state): State<AppState>,
    Json(payload): Json<BasicRequest>,
) -> Json<HeartbeatResponse> {
    let user_id = payload.user_id;
    let is_auto_lie = payload.program.as_deref() == Some("auto_lie");

    println!(
        ">>> Heartbeat from: {}, program: {:?}",
        user_id, payload.program
    );

    // 핑 업데이트
    if is_auto_lie {
        let _ = sqlx::query(
            "UPDATE users SET auto_lie_last_ping = NOW() WHERE user_id = ? AND auto_lie_login = 1",
        )
        .bind(&user_id)
        .execute(&state.user_db)
        .await;
    } else {
        let _ =
            sqlx::query("UPDATE users SET last_ping = NOW() WHERE user_id = ? AND is_login = 1")
                .bind(&user_id)
                .execute(&state.user_db)
                .await;
    }

    if let Ok(row) = sqlx::query(
        "SELECT auto_lie_login, is_login, expire_date, auto_lie FROM users WHERE user_id = ?",
    )
    .bind(&user_id)
    .fetch_one(&state.user_db)
    .await
    {
        let auto_lie_login: i8 = row.get("auto_lie_login");
        let is_login: i8 = row.try_get("is_login").unwrap_or(0);
        let auto_lie: i8 = row.get("auto_lie");
        let expire_date: chrono::NaiveDateTime = row.get("expire_date");

        // 권한 또는 세션 체크
        if is_auto_lie {
            if auto_lie == 0 || auto_lie_login == 0 {
                return Json(HeartbeatResponse {
                    action: Some("kick".to_string()),
                });
            }
        } else {
            if is_login == 0 {
                return Json(HeartbeatResponse {
                    action: Some("kick".to_string()),
                });
            }
        }

        // 만료 체크
        let expire_edmonton = chrono_tz::America::Edmonton
            .from_local_datetime(&expire_date)
            .single()
            .unwrap();
        if Utc::now().with_timezone(&chrono_tz::America::Edmonton) >= expire_edmonton {
            if is_auto_lie {
                let _ = sqlx::query("UPDATE users SET auto_lie_login = 0 WHERE user_id = ?")
                    .bind(&user_id)
                    .execute(&state.user_db)
                    .await;
            } else {
                let _ = sqlx::query("UPDATE users SET is_login = 0 WHERE user_id = ?")
                    .bind(&user_id)
                    .execute(&state.user_db)
                    .await;
            }
            return Json(HeartbeatResponse {
                action: Some("kick".to_string()),
            });
        }
    }
    Json(HeartbeatResponse { action: None })
}

/// 4. 사용자 추가
async fn add_user_handler(
    State(state): State<AppState>,
    Json(payload): Json<AddUserRequest>,
) -> Json<AddUserResponse> {
    let user_id = payload.user_id;
    let days = payload.days;
    let who_added = payload.who_added;
    let discord_tele_id = payload.discord_tele_id;

    let expire_date = Utc::now().with_timezone(&chrono_tz::America::Edmonton)
        + chrono::Duration::days(days as i64);
    let expire_str = expire_date.naive_local();

    let query = "
        INSERT INTO users (user_id, expire_date, who_added, Discord_tele_id, last_ping, expire, auto_lie, auto_lie_login, is_login)
        VALUES (?, ?, ?, ?, NOW(), 'no', 0, 0, 0)
    ";

    let res = sqlx::query(query)
        .bind(&user_id)
        .bind(expire_str)
        .bind(&who_added)
        .bind(&discord_tele_id)
        .execute(&state.seller_db)
        .await;

    match res {
        Ok(_) => Json(AddUserResponse {
            status: "ok".to_string(),
            message: format!("성공적으로 추가되었습니다: {} ({}일)", user_id, days),
        }),
        Err(e) => {
            let err_msg = e.to_string();
            let final_msg = if err_msg.contains("Duplicate entry") {
                format!("실패: '{}'은(는) 이미 존재하는 사용자입니다.", user_id)
            } else {
                format!("사용자 추가 실패: {}", e)
            };

            println!("DB Error (AddUser): {:?}", e);
            Json(AddUserResponse {
                status: "error".to_string(),
                message: final_msg,
            })
        }
    }
}

/// 5. [NEW] 특정 유저의 auto_lie 권한 활성화/비활성화
async fn update_auto_lie_handler(
    State(state): State<AppState>,
    Json(payload): Json<UpdateAutoLieRequest>,
) -> Json<AddUserResponse> {
    let res = sqlx::query("UPDATE users SET auto_lie = ? WHERE user_id = ?")
        .bind(payload.enable)
        .bind(&payload.user_id)
        .execute(&state.seller_db)
        .await;

    match res {
        Ok(result) => {
            if result.rows_affected() == 0 {
                return Json(AddUserResponse {
                    status: "error".to_string(),
                    message: format!("존재하지 않는 사용자입니다: {}", payload.user_id),
                });
            }
            Json(AddUserResponse {
                status: "ok".to_string(),
                message: format!(
                    "유저 [{}]의 auto_lie 권한이 '{}'으로 변경되었습니다.",
                    payload.user_id,
                    if payload.enable == 1 {
                        "활성"
                    } else {
                        "비활성"
                    }
                ),
            })
        }
        Err(e) => {
            println!("DB Error (UpdateAutoLie): {:?}", e);
            Json(AddUserResponse {
                status: "error".to_string(),
                message: format!("권한 변경 중 서버 오류 발생: {}", e),
            })
        }
    }
}
