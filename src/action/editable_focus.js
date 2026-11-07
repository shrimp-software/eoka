(() => {
    if (document.designMode === 'on') return true;
    let active = document.activeElement;
    while (active?.shadowRoot?.activeElement) active = active.shadowRoot.activeElement;
    if (!active) return false;
    if (active.isContentEditable || active.localName === 'iframe' || active.localName === 'frame') return true;
    if (active.disabled || active.readOnly) return false;
    if (active.localName === 'textarea') return true;
    return active.localName === 'input'
        && !['button', 'checkbox', 'color', 'file', 'hidden', 'image', 'radio', 'range', 'reset', 'submit'].includes(active.type);
})()
