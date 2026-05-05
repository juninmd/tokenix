/**
 * PostgreSQL repository layer.
 * Generic base + concrete UserRepository and PostRepository.
 * Uses pg (node-postgres) with connection pooling and prepared statements.
 */

import { Pool, PoolClient, QueryResult } from "pg";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface PaginationParams {
  page: number;
  pageSize: number;
}

export interface PaginatedResult<T> {
  items: T[];
  total: number;
  page: number;
  pageSize: number;
  totalPages: number;
}

export interface User {
  id: string;
  email: string;
  username: string;
  passwordHash: string;
  role: "guest" | "user" | "moderator" | "admin";
  createdAt: Date;
  updatedAt: Date;
  deletedAt: Date | null;
}

export interface Post {
  id: string;
  authorId: string;
  title: string;
  slug: string;
  body: string;
  published: boolean;
  publishedAt: Date | null;
  createdAt: Date;
  updatedAt: Date;
}

export type CreateUserInput = Pick<User, "email" | "username" | "passwordHash" | "role">;
export type UpdateUserInput = Partial<Pick<User, "username" | "role">>;
export type CreatePostInput = Pick<Post, "authorId" | "title" | "slug" | "body">;
export type UpdatePostInput = Partial<Pick<Post, "title" | "slug" | "body" | "published">>;

// ---------------------------------------------------------------------------
// Connection pool singleton
// ---------------------------------------------------------------------------

let _pool: Pool | null = null;

export function getPool(): Pool {
  if (!_pool) {
    _pool = new Pool({
      connectionString: process.env.DATABASE_URL,
      max: 20,
      idleTimeoutMillis: 30_000,
      connectionTimeoutMillis: 5_000,
    });
    _pool.on("error", (err) => {
      console.error("[pg] Unexpected idle client error:", err);
    });
  }
  return _pool;
}

export async function closePool(): Promise<void> {
  if (_pool) {
    await _pool.end();
    _pool = null;
  }
}

// ---------------------------------------------------------------------------
// Transaction helper
// ---------------------------------------------------------------------------

export async function withTransaction<T>(
  pool: Pool,
  fn: (client: PoolClient) => Promise<T>
): Promise<T> {
  const client = await pool.connect();
  try {
    await client.query("BEGIN");
    const result = await fn(client);
    await client.query("COMMIT");
    return result;
  } catch (err) {
    await client.query("ROLLBACK");
    throw err;
  } finally {
    client.release();
  }
}

// ---------------------------------------------------------------------------
// Base repository
// ---------------------------------------------------------------------------

export abstract class BaseRepository<T, CreateInput, UpdateInput> {
  protected readonly pool: Pool;
  protected abstract readonly tableName: string;
  protected abstract readonly columns: string[];

  constructor(pool: Pool) {
    this.pool = pool;
  }

  protected colList(): string {
    return this.columns.join(", ");
  }

  protected placeholders(offset = 1): string {
    return this.columns.map((_, i) => `$${i + offset}`).join(", ");
  }

  async findById(id: string): Promise<T | null> {
    const { rows } = await this.pool.query<T>(
      `SELECT ${this.colList()} FROM ${this.tableName} WHERE id = $1 AND deleted_at IS NULL`,
      [id]
    );
    return rows[0] ?? null;
  }

  async findAll({ page, pageSize }: PaginationParams): Promise<PaginatedResult<T>> {
    const offset = (page - 1) * pageSize;
    const [dataResult, countResult]: [QueryResult<T>, QueryResult<{ count: string }>] =
      await Promise.all([
        this.pool.query<T>(
          `SELECT ${this.colList()} FROM ${this.tableName}
           WHERE deleted_at IS NULL
           ORDER BY created_at DESC
           LIMIT $1 OFFSET $2`,
          [pageSize, offset]
        ),
        this.pool.query<{ count: string }>(
          `SELECT COUNT(*) FROM ${this.tableName} WHERE deleted_at IS NULL`
        ),
      ]);
    const total = parseInt(countResult.rows[0].count, 10);
    return {
      items: dataResult.rows,
      total,
      page,
      pageSize,
      totalPages: Math.ceil(total / pageSize),
    };
  }

  abstract create(input: CreateInput, client?: PoolClient): Promise<T>;
  abstract update(id: string, input: UpdateInput, client?: PoolClient): Promise<T | null>;

  async softDelete(id: string, client?: PoolClient): Promise<boolean> {
    const exec = client ?? this.pool;
    const { rowCount } = await exec.query(
      `UPDATE ${this.tableName} SET deleted_at = NOW() WHERE id = $1 AND deleted_at IS NULL`,
      [id]
    );
    return (rowCount ?? 0) > 0;
  }
}

// ---------------------------------------------------------------------------
// UserRepository
// ---------------------------------------------------------------------------

export class UserRepository extends BaseRepository<User, CreateUserInput, UpdateUserInput> {
  protected readonly tableName = "users";
  protected readonly columns = [
    "id", "email", "username", "password_hash", "role",
    "created_at", "updated_at", "deleted_at",
  ];

  async create(input: CreateUserInput, client?: PoolClient): Promise<User> {
    const exec = client ?? this.pool;
    const { rows } = await exec.query<User>(
      `INSERT INTO users (email, username, password_hash, role, created_at, updated_at)
       VALUES ($1, $2, $3, $4, NOW(), NOW())
       RETURNING ${this.colList()}`,
      [input.email, input.username, input.passwordHash, input.role]
    );
    return rows[0];
  }

  async update(id: string, input: UpdateUserInput, client?: PoolClient): Promise<User | null> {
    const exec = client ?? this.pool;
    const sets: string[] = [];
    const values: unknown[] = [];
    if (input.username !== undefined) {
      sets.push(`username = $${values.length + 1}`);
      values.push(input.username);
    }
    if (input.role !== undefined) {
      sets.push(`role = $${values.length + 1}`);
      values.push(input.role);
    }
    if (sets.length === 0) return this.findById(id);
    sets.push("updated_at = NOW()");
    values.push(id);
    const { rows } = await exec.query<User>(
      `UPDATE users SET ${sets.join(", ")}
       WHERE id = $${values.length} AND deleted_at IS NULL
       RETURNING ${this.colList()}`,
      values
    );
    return rows[0] ?? null;
  }

  async findByEmail(email: string): Promise<User | null> {
    const { rows } = await this.pool.query<User>(
      `SELECT ${this.colList()} FROM users WHERE email = $1 AND deleted_at IS NULL`,
      [email]
    );
    return rows[0] ?? null;
  }

  async findByRole(role: User["role"], pagination: PaginationParams): Promise<PaginatedResult<User>> {
    const offset = (pagination.page - 1) * pagination.pageSize;
    const [data, count] = await Promise.all([
      this.pool.query<User>(
        `SELECT ${this.colList()} FROM users
         WHERE role = $1 AND deleted_at IS NULL
         ORDER BY created_at DESC LIMIT $2 OFFSET $3`,
        [role, pagination.pageSize, offset]
      ),
      this.pool.query<{ count: string }>(
        `SELECT COUNT(*) FROM users WHERE role = $1 AND deleted_at IS NULL`,
        [role]
      ),
    ]);
    const total = parseInt(count.rows[0].count, 10);
    return {
      items: data.rows,
      total,
      page: pagination.page,
      pageSize: pagination.pageSize,
      totalPages: Math.ceil(total / pagination.pageSize),
    };
  }
}

// ---------------------------------------------------------------------------
// PostRepository
// ---------------------------------------------------------------------------

export class PostRepository extends BaseRepository<Post, CreatePostInput, UpdatePostInput> {
  protected readonly tableName = "posts";
  protected readonly columns = [
    "id", "author_id", "title", "slug", "body",
    "published", "published_at", "created_at", "updated_at",
  ];

  async create(input: CreatePostInput, client?: PoolClient): Promise<Post> {
    const exec = client ?? this.pool;
    const { rows } = await exec.query<Post>(
      `INSERT INTO posts (author_id, title, slug, body, published, created_at, updated_at)
       VALUES ($1, $2, $3, $4, FALSE, NOW(), NOW())
       RETURNING ${this.colList()}`,
      [input.authorId, input.title, input.slug, input.body]
    );
    return rows[0];
  }

  async update(id: string, input: UpdatePostInput, client?: PoolClient): Promise<Post | null> {
    const exec = client ?? this.pool;
    const sets: string[] = [];
    const values: unknown[] = [];
    const fields: [keyof UpdatePostInput, string][] = [
      ["title", "title"],
      ["slug", "slug"],
      ["body", "body"],
      ["published", "published"],
    ];
    for (const [key, col] of fields) {
      if (input[key] !== undefined) {
        sets.push(`${col} = $${values.length + 1}`);
        values.push(input[key]);
      }
    }
    if (input.published === true) {
      sets.push(`published_at = COALESCE(published_at, NOW())`);
    }
    if (sets.length === 0) return this.findById(id);
    sets.push("updated_at = NOW()");
    values.push(id);
    const { rows } = await exec.query<Post>(
      `UPDATE posts SET ${sets.join(", ")}
       WHERE id = $${values.length} AND deleted_at IS NULL
       RETURNING ${this.colList()}`,
      values
    );
    return rows[0] ?? null;
  }

  async findBySlug(slug: string): Promise<Post | null> {
    const { rows } = await this.pool.query<Post>(
      `SELECT ${this.colList()} FROM posts WHERE slug = $1`,
      [slug]
    );
    return rows[0] ?? null;
  }

  async findPublished(pagination: PaginationParams): Promise<PaginatedResult<Post>> {
    const offset = (pagination.page - 1) * pagination.pageSize;
    const [data, count] = await Promise.all([
      this.pool.query<Post>(
        `SELECT ${this.colList()} FROM posts
         WHERE published = TRUE
         ORDER BY published_at DESC LIMIT $1 OFFSET $2`,
        [pagination.pageSize, offset]
      ),
      this.pool.query<{ count: string }>(
        `SELECT COUNT(*) FROM posts WHERE published = TRUE`
      ),
    ]);
    const total = parseInt(count.rows[0].count, 10);
    return {
      items: data.rows,
      total,
      page: pagination.page,
      pageSize: pagination.pageSize,
      totalPages: Math.ceil(total / pagination.pageSize),
    };
  }

  async findByAuthor(authorId: string, pagination: PaginationParams): Promise<PaginatedResult<Post>> {
    const offset = (pagination.page - 1) * pagination.pageSize;
    const [data, count] = await Promise.all([
      this.pool.query<Post>(
        `SELECT ${this.colList()} FROM posts
         WHERE author_id = $1
         ORDER BY created_at DESC LIMIT $2 OFFSET $3`,
        [authorId, pagination.pageSize, offset]
      ),
      this.pool.query<{ count: string }>(
        `SELECT COUNT(*) FROM posts WHERE author_id = $1`,
        [authorId]
      ),
    ]);
    const total = parseInt(count.rows[0].count, 10);
    return {
      items: data.rows,
      total,
      page: pagination.page,
      pageSize: pagination.pageSize,
      totalPages: Math.ceil(total / pagination.pageSize),
    };
  }
}
