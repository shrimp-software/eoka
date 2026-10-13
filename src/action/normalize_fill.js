function(value) {
    const probe = this.cloneNode(false);
    probe.value = value;
    if (this.type === 'range') {
        return Number.isFinite(Number(value)) && probe.validity.valid && Number(probe.value) === Number(value) ? probe.value : null;
    }
    return value === '' || probe.value !== '' ? probe.value : null;
}
