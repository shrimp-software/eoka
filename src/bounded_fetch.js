const controller = new AbortController();
const timer = setTimeout(() => controller.abort(), 25000);
let reader;
try {
    const response = await fetch(request.url, {
        method: request.method,
        headers: request.headers,
        body: request.body ?? undefined,
        redirect: request.redirect,
        credentials: 'include',
        signal: controller.signal,
    });
    const result = {
        url: response.url, status: response.status, statusText: response.statusText,
        type: response.type, redirected: response.redirected,
        headers: Object.fromEntries(response.headers.entries()),
        body: '', truncated: false, bytesRead: 0, bodyOmitted: null,
    };
    const mime = (response.headers.get('content-type') ?? '').split(';')[0].trim().toLowerCase();
    const binary = /^(image|audio|video|font)\//.test(mime)
        || /^(application\/(octet-stream|zip|gzip|x-gzip|pdf|wasm|x-7z-compressed|x-rar-compressed|vnd\.rar|x-tar|x-bzip2))$/.test(mime);
    if (request.maxBytes === 0) result.bodyOmitted = 'headers_only';
    else if (binary) result.bodyOmitted = 'binary';
    else if (response.type === 'opaque' || response.type === 'opaqueredirect') result.bodyOmitted = 'opaque';
    if (result.bodyOmitted !== null) {
        controller.abort();
        return result;
    }
    if (response.body) {
        reader = response.body.getReader();
        const decoder = new TextDecoder();
        const buffer = new Uint8Array(request.maxBytes);
        while (true) {
            const {done, value} = await reader.read();
            if (done) break;
            const remaining = request.maxBytes - result.bytesRead;
            const kept = value.subarray(0, remaining);
            buffer.set(kept, result.bytesRead);
            result.bytesRead += kept.byteLength;
            if (result.bytesRead === request.maxBytes) {
                result.truncated = true;
                controller.abort();
                break;
            }
        }
        result.body = decoder.decode(buffer.subarray(0, result.bytesRead), {stream: result.truncated});
    }
    if (new TextEncoder().encode(JSON.stringify(result)).byteLength > 60 * 1024 * 1024) {
        throw new Error('fetch_response_too_large: escaped response exceeds protocol budget; reduce max_body');
    }
    return result;
} finally {
    clearTimeout(timer);
    controller.abort();
    if (reader) {
        try { await reader.cancel(); } catch {} finally { reader.releaseLock(); }
    }
}
