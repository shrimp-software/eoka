function(query) {
    if (query.kind === 'css') return Array.from(document.querySelectorAll(query.selector));
    if (query.kind === 'placeholder') return Array.from(document.querySelectorAll('[placeholder]')).filter(el => el.getAttribute('placeholder') === query.text);
    if (query.kind === 'role') return Array.from(document.querySelectorAll('[role]')).filter(el => el.getAttribute('role') === query.role);
    if (query.kind !== 'text') throw new Error('Unsupported locator');
    return (TEXT_CANDIDATES)(query.text, query.exact ? 'exact' : 'contains', false);
}
