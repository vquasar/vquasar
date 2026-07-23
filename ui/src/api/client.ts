// Thin typed fetch client for the /api/v1 surface. Surfaces the control plane's
// {error:{code,message,request_id}} envelope (design section 37) as an Error.

const BASE = "/api/v1";

export interface ApiErrorBody {
  error: { code: string; message: string; request_id: string };
}

export class ApiError extends Error {
  code: string;
  requestId?: string;
  constructor(message: string, code: string, requestId?: string) {
    super(message);
    this.name = "ApiError";
    this.code = code;
    this.requestId = requestId;
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
    headers: { "content-type": "application/json" },
    ...init,
  });
  if (!res.ok) {
    let code = `HTTP_${res.status}`;
    let message = res.statusText;
    let requestId: string | undefined;
    try {
      const body = (await res.json()) as ApiErrorBody;
      if (body?.error) {
        code = body.error.code;
        message = body.error.message;
        requestId = body.error.request_id;
      }
    } catch {
      // non-JSON error body; keep defaults
    }
    throw new ApiError(message, code, requestId);
  }
  if (res.status === 204) {
    return undefined as T;
  }
  return (await res.json()) as T;
}

export const api = {
  get: <T>(path: string) => request<T>(path),
  post: <T>(path: string, body?: unknown) =>
    request<T>(path, { method: "POST", body: body ? JSON.stringify(body) : undefined }),
  del: <T>(path: string) => request<T>(path, { method: "DELETE" }),
};
