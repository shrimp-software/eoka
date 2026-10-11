
// Ensure Intl.DateTimeFormat returns consistent timezone
const targetTimezone = '__EOKA_TIMEZONE__';
const origDateTimeFormat = Intl.DateTimeFormat;

Intl.DateTimeFormat = _eoka_mark(function DateTimeFormat(locales, options) {
    const merged = options ? Object.assign({}, options) : {};
    if (!merged.timeZone) merged.timeZone = targetTimezone;
    return new origDateTimeFormat(locales, merged);
}, 'DateTimeFormat');
Intl.DateTimeFormat.prototype = origDateTimeFormat.prototype;
Intl.DateTimeFormat.supportedLocalesOf = origDateTimeFormat.supportedLocalesOf;

// Override Date.prototype.getTimezoneOffset to match the target timezone.
// Precompute offsets for Jan and Jul to handle DST, avoiding per-call
// toLocaleString() which is ~10x slower than native and timing-detectable.
{
    const _origGetTZO = Date.prototype.getTimezoneOffset;
    function _calcOffset(date) {
        const utcStr = date.toLocaleString('en-US', { timeZone: 'UTC' });
        const tzStr = date.toLocaleString('en-US', { timeZone: targetTimezone });
        return (new Date(utcStr) - new Date(tzStr)) / 60000;
    }
    const year = new Date().getFullYear();
    const janOffset = _calcOffset(new Date(year, 0, 15));
    const julOffset = _calcOffset(new Date(year, 6, 15));
    // Standard offset is the larger value (more positive = further west)
    const stdOffset = Math.max(janOffset, julOffset);
    const dstOffset = Math.min(janOffset, julOffset);
    // If offsets differ, this timezone observes DST
    const hasDST = janOffset !== julOffset;
    // Which months are DST depends on hemisphere: if Jul is the smaller
    // offset, DST is northern-hemisphere (Mar-Nov); otherwise southern.
    const dstIsJul = julOffset < janOffset;

    Date.prototype.getTimezoneOffset = _eoka_mark(function getTimezoneOffset() {
        if (!hasDST) return stdOffset;
        // Quick month-based check — accurate for the vast majority of dates.
        const m = this.getUTCMonth();
        const isDST = dstIsJul ? (m >= 2 && m <= 10) : (m <= 2 || m >= 10);
        return isDST ? dstOffset : stdOffset;
    }, 'getTimezoneOffset');
}
