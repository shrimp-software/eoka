function(query) {
    if (query.kind === 'css') return Array.from(document.querySelectorAll(query.selector));
    if (query.kind === 'placeholder') return Array.from(document.querySelectorAll('[placeholder]')).filter(el => el.getAttribute('placeholder') === query.text);
    if (query.kind === 'role') return Array.from(document.querySelectorAll('[role]')).filter(el => el.getAttribute('role') === query.role);
    if (query.kind !== 'text') throw new Error('Unsupported locator');
    const matches = el => {
        const text = (el.innerText || el.textContent || el.value || '').trim();
        return query.exact ? text === query.text : text.toLowerCase().includes(query.text.toLowerCase());
    };
    const interactive = Array.from(document.querySelectorAll('a,button,input[type="submit"],input[type="button"],[role="button"],[onclick]')).filter(matches);
    if (interactive.length) return interactive;
    return Array.from(document.querySelectorAll('label,span,div,p,h1,h2,h3,h4,h5,h6,li,td,th')).filter(matches).filter(el => !Array.from(el.children).some(matches));
}
