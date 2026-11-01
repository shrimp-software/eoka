async (expression, maximumBytes) => {
    const value = await (0, eval)(expression);
    const ancestors = new WeakSet();
    const normalize = (value) => {
        if (value === null || typeof value === "string" || typeof value === "boolean") return value;
        if (typeof value === "number") {
            if (!Number.isFinite(value) || (Number.isInteger(value) && !Number.isSafeInteger(value))) {
                throw new TypeError("Evaluation result contains a non-finite or unsafe number");
            }
            return value;
        }
        if (typeof value !== "object") throw new TypeError("Evaluation result is not JSON-compatible");
        const prototype = Object.getPrototypeOf(value);
        if (!Array.isArray(value) && prototype !== Object.prototype && prototype !== null) {
            throw new TypeError("Evaluation result contains a non-JSON object");
        }
        if (Object.getOwnPropertySymbols(value).length !== 0) {
            throw new TypeError("Evaluation result contains symbol keys");
        }
        if (ancestors.has(value)) throw new TypeError("Evaluation result contains a cycle");
        ancestors.add(value);
        const normalized = Array.isArray(value) ? [] : Object.create(null);
        if (Array.isArray(value)) {
            for (let index = 0; index < value.length; index++) {
                if (!Object.hasOwn(value, index)) throw new TypeError("Evaluation result contains an array hole");
                normalized.push(normalize(value[index]));
            }
        } else {
            for (const key of Object.keys(value)) normalized[key] = normalize(value[key]);
        }
        ancestors.delete(value);
        return normalized;
    };
    const normalized = normalize(value);
    const serialized = JSON.stringify(normalized);
    if (serialized.length > maximumBytes || new TextEncoder().encode(serialized).byteLength > maximumBytes) {
        return {kind: "too_large"};
    }
    return {kind: "value", value: normalized};
}
