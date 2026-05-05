//! User domain service.
//! Handles registration, profile updates, password management, and account deletion.
//! All mutations go through this service; repositories are injected for testability.

use std::sync::Arc;

use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub username: String,
    #[serde(skip_serializing)]
    pub password_hash: String,
    pub role: Role,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Guest,
    User,
    Moderator,
    Admin,
}

impl Role {
    pub fn level(&self) -> u8 {
        match self {
            Role::Guest => 0,
            Role::User => 1,
            Role::Moderator => 2,
            Role::Admin => 3,
        }
    }

    pub fn can_promote_to(&self, target: &Role) -> bool {
        self.level() >= target.level()
    }
}

// ---------------------------------------------------------------------------
// Repository trait (injected)
// ---------------------------------------------------------------------------

#[async_trait]
pub trait UserRepo: Send + Sync {
    async fn find_by_id(&self, id: Uuid) -> Result<Option<User>>;
    async fn find_by_email(&self, email: &str) -> Result<Option<User>>;
    async fn find_by_username(&self, username: &str) -> Result<Option<User>>;
    async fn list(&self, page: u32, page_size: u32) -> Result<(Vec<User>, u64)>;
    async fn create(&self, user: &User) -> Result<()>;
    async fn update(&self, user: &User) -> Result<()>;
    async fn soft_delete(&self, id: Uuid) -> Result<bool>;
}

// ---------------------------------------------------------------------------
// Password hashing (abstraced for testing)
// ---------------------------------------------------------------------------

pub trait PasswordHasher: Send + Sync {
    fn hash(&self, password: &str) -> Result<String>;
    fn verify(&self, password: &str, hash: &str) -> Result<bool>;
}

// ---------------------------------------------------------------------------
// Input types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RegisterInput {
    pub email: String,
    pub username: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateProfileInput {
    pub username: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ChangePasswordInput {
    pub current_password: String,
    pub new_password: String,
}

#[derive(Debug, Deserialize)]
pub struct PromoteInput {
    pub target_id: Uuid,
    pub new_role: Role,
}

// ---------------------------------------------------------------------------
// Pagination
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: u64,
    pub page: u32,
    pub page_size: u32,
    pub total_pages: u32,
}

impl<T> Page<T> {
    pub fn new(items: Vec<T>, total: u64, page: u32, page_size: u32) -> Self {
        let total_pages = ((total as f64) / (page_size as f64)).ceil() as u32;
        Page { items, total, page, page_size, total_pages }
    }
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

pub struct UserService {
    repo: Arc<dyn UserRepo>,
    hasher: Arc<dyn PasswordHasher>,
    min_password_len: usize,
}

impl UserService {
    pub fn new(repo: Arc<dyn UserRepo>, hasher: Arc<dyn PasswordHasher>) -> Self {
        Self { repo, hasher, min_password_len: 8 }
    }

    pub async fn register(&self, input: RegisterInput) -> Result<User> {
        self.validate_email(&input.email)?;
        self.validate_username(&input.username)?;
        self.validate_password(&input.password)?;

        if self.repo.find_by_email(&input.email).await?.is_some() {
            bail!("email already registered");
        }
        if self.repo.find_by_username(&input.username).await?.is_some() {
            bail!("username already taken");
        }

        let user = User {
            id: Uuid::new_v4(),
            email: input.email.to_lowercase().trim().to_string(),
            username: input.username.trim().to_string(),
            password_hash: self.hasher.hash(&input.password)?,
            role: Role::User,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            deleted_at: None,
        };
        self.repo.create(&user).await?;
        Ok(user)
    }

    pub async fn get_by_id(&self, id: Uuid) -> Result<User> {
        self.repo
            .find_by_id(id)
            .await?
            .ok_or_else(|| anyhow!("user not found"))
    }

    pub async fn list(&self, page: u32, page_size: u32) -> Result<Page<User>> {
        let page_size = page_size.clamp(1, 100);
        let (users, total) = self.repo.list(page.max(1), page_size).await?;
        Ok(Page::new(users, total, page, page_size))
    }

    pub async fn update_profile(&self, id: Uuid, input: UpdateProfileInput) -> Result<User> {
        let mut user = self.get_by_id(id).await?;

        if let Some(ref username) = input.username {
            self.validate_username(username)?;
            if self.repo.find_by_username(username).await?.map(|u| u.id != id).unwrap_or(false) {
                bail!("username already taken");
            }
            user.username = username.trim().to_string();
        }

        user.updated_at = Utc::now();
        self.repo.update(&user).await?;
        Ok(user)
    }

    pub async fn change_password(&self, id: Uuid, input: ChangePasswordInput) -> Result<()> {
        let mut user = self.get_by_id(id).await?;

        if !self.hasher.verify(&input.current_password, &user.password_hash)? {
            bail!("current password is incorrect");
        }
        self.validate_password(&input.new_password)?;

        user.password_hash = self.hasher.hash(&input.new_password)?;
        user.updated_at = Utc::now();
        self.repo.update(&user).await?;
        Ok(())
    }

    pub async fn promote(&self, actor: &User, input: PromoteInput) -> Result<User> {
        if !actor.role.can_promote_to(&input.new_role) {
            bail!("insufficient permissions to assign role {:?}", input.new_role);
        }
        let mut target = self.get_by_id(input.target_id).await?;
        target.role = input.new_role;
        target.updated_at = Utc::now();
        self.repo.update(&target).await?;
        Ok(target)
    }

    pub async fn delete(&self, actor: &User, target_id: Uuid) -> Result<()> {
        if actor.id != target_id && actor.role != Role::Admin {
            bail!("insufficient permissions");
        }
        let deleted = self.repo.soft_delete(target_id).await?;
        if !deleted {
            bail!("user not found");
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Validation helpers
    // -----------------------------------------------------------------------

    fn validate_email(&self, email: &str) -> Result<()> {
        let e = email.trim();
        if e.is_empty() {
            bail!("email cannot be empty");
        }
        if !e.contains('@') || !e.contains('.') {
            bail!("email format is invalid");
        }
        if e.len() > 254 {
            bail!("email too long");
        }
        Ok(())
    }

    fn validate_username(&self, username: &str) -> Result<()> {
        let u = username.trim();
        if u.is_empty() {
            bail!("username cannot be empty");
        }
        if u.len() < 3 || u.len() > 32 {
            bail!("username must be 3–32 characters");
        }
        if !u.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
            bail!("username may only contain letters, numbers, _ and -");
        }
        Ok(())
    }

    fn validate_password(&self, password: &str) -> Result<()> {
        if password.len() < self.min_password_len {
            bail!("password must be at least {} characters", self.min_password_len);
        }
        let has_upper = password.chars().any(|c| c.is_uppercase());
        let has_digit = password.chars().any(|c| c.is_ascii_digit());
        if !has_upper || !has_digit {
            bail!("password must contain at least one uppercase letter and one digit");
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tokio::sync::Mutex;

    struct InMemoryRepo {
        users: Mutex<HashMap<Uuid, User>>,
    }

    impl InMemoryRepo {
        fn new() -> Arc<Self> {
            Arc::new(Self { users: Mutex::new(HashMap::new()) })
        }
    }

    #[async_trait]
    impl UserRepo for InMemoryRepo {
        async fn find_by_id(&self, id: Uuid) -> Result<Option<User>> {
            Ok(self.users.lock().await.get(&id).cloned())
        }
        async fn find_by_email(&self, email: &str) -> Result<Option<User>> {
            Ok(self.users.lock().await.values().find(|u| u.email == email).cloned())
        }
        async fn find_by_username(&self, username: &str) -> Result<Option<User>> {
            Ok(self.users.lock().await.values().find(|u| u.username == username).cloned())
        }
        async fn list(&self, page: u32, page_size: u32) -> Result<(Vec<User>, u64)> {
            let all: Vec<_> = self.users.lock().await.values().cloned().collect();
            let total = all.len() as u64;
            let start = ((page - 1) * page_size) as usize;
            Ok((all.into_iter().skip(start).take(page_size as usize).collect(), total))
        }
        async fn create(&self, user: &User) -> Result<()> {
            self.users.lock().await.insert(user.id, user.clone());
            Ok(())
        }
        async fn update(&self, user: &User) -> Result<()> {
            self.users.lock().await.insert(user.id, user.clone());
            Ok(())
        }
        async fn soft_delete(&self, id: Uuid) -> Result<bool> {
            Ok(self.users.lock().await.remove(&id).is_some())
        }
    }

    struct PlainHasher;
    impl PasswordHasher for PlainHasher {
        fn hash(&self, p: &str) -> Result<String> { Ok(format!("hash:{p}")) }
        fn verify(&self, p: &str, h: &str) -> Result<bool> { Ok(h == format!("hash:{p}")) }
    }

    fn make_service() -> UserService {
        UserService::new(InMemoryRepo::new(), Arc::new(PlainHasher))
    }

    #[tokio::test]
    async fn test_register_success() {
        let svc = make_service();
        let user = svc.register(RegisterInput {
            email: "alice@example.com".into(),
            username: "alice".into(),
            password: "Password1".into(),
        }).await.unwrap();
        assert_eq!(user.email, "alice@example.com");
        assert_eq!(user.role, Role::User);
    }

    #[tokio::test]
    async fn test_register_duplicate_email() {
        let svc = make_service();
        let input = || RegisterInput {
            email: "bob@example.com".into(),
            username: "bob".into(),
            password: "Password1".into(),
        };
        svc.register(input()).await.unwrap();
        let err = svc.register(input()).await.unwrap_err();
        assert!(err.to_string().contains("already registered"));
    }

    #[tokio::test]
    async fn test_change_password_wrong_current() {
        let svc = make_service();
        let user = svc.register(RegisterInput {
            email: "carol@example.com".into(),
            username: "carol".into(),
            password: "Password1".into(),
        }).await.unwrap();
        let err = svc.change_password(user.id, ChangePasswordInput {
            current_password: "Wrong1234".into(),
            new_password: "NewPass1".into(),
        }).await.unwrap_err();
        assert!(err.to_string().contains("incorrect"));
    }
}
