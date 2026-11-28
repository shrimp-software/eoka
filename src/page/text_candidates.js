function(text, mode, all) {
    // Shared by find_by_text, find_all_by_text and text locators. Returns
    // matching interactive elements when there are any; otherwise the
    // innermost matching static elements, never their ancestors. With `all`,
    // both, minus static elements that contain or sit inside an interactive
    // match.
    const needle = mode === 'exact' ? text : text.toLowerCase();
    const matches = el => {
        const raw = (el.innerText || el.textContent || el.value || '').trim();
        if (mode === 'exact') return raw === needle;
        const t = raw.toLowerCase();
        if (mode === 'starts_with') return t.startsWith(needle);
        if (mode === 'ends_with') return t.endsWith(needle);
        return t.includes(needle);
    };
    const interactive = Array.from(document.querySelectorAll('a,button,input[type="submit"],input[type="button"],[role="button"],[onclick]')).filter(matches);
    if (interactive.length && !all) return interactive;
    const innermost = Array.from(document.querySelectorAll('label,span,div,p,h1,h2,h3,h4,h5,h6,li,td,th'))
        .filter(matches)
        .filter(el => !Array.from(el.children).some(matches))
        .filter(el => !interactive.some(i => el.contains(i) || i.contains(el)));
    return interactive.concat(innermost);
}
