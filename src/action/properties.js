function() {
    const rect = this.getBoundingClientRect();
    const style = getComputedStyle(this);
    const visible = this.isConnected && rect.width > 0 && rect.height > 0 && style.visibility !== 'hidden' && style.visibility !== 'collapse';
    const disabled = this.matches(':disabled') || !!this.closest('[aria-disabled="true"],[inert]');
    const editable = !disabled && !this.readOnly && this.getAttribute('aria-readonly') !== 'true' && (this.isContentEditable || this instanceof HTMLTextAreaElement || (this instanceof HTMLInputElement && ["text","search","url","tel","email","password","number","range"].includes(this.type)));
    const x = (Math.max(0,rect.left) + Math.min(innerWidth,rect.right))/2;
    const y = (Math.max(0,rect.top) + Math.min(innerHeight,rect.bottom))/2;
    let hit = this.ownerDocument.elementFromPoint(x,y);
    for(let depth=0; hit?.shadowRoot && depth<64; depth++) {
        const inner = hit.shadowRoot.elementFromPoint(x,y);
        if(inner === hit) break;
        hit = inner;
    }
    const receives = visible && x>=0 && y>=0 && x<innerWidth && y<innerHeight && (hit === this || this.contains(hit));
    let active = this.ownerDocument.activeElement;
    while(active?.shadowRoot?.activeElement) active = active.shadowRoot.activeElement;
    return {connected:this.isConnected,visible,disabled,editable,receives,x,y,
        rect:[rect.left,rect.top,rect.width,rect.height],value:this.isContentEditable ? this.textContent : this.value,
        text:this.innerText || this.textContent || '',checked:this.checked === true,
        selected:this.selectedOptions ? Array.from(this.selectedOptions,o=>o.value) : [],
        files:this.files ? Array.from(this.files,f=>({name:f.name,size:f.size})) : [],
        focused:active===this,hovered:this.matches(':hover'),type:this.type || '',
        inViewport:rect.bottom>0 && rect.right>0 && rect.top<innerHeight && rect.left<innerWidth};
}
