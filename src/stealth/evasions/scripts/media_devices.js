
if (typeof MediaDevices !== 'undefined' && MediaDevices.prototype.enumerateDevices) {
    const fakeDevices = __EOKA_MEDIA_DEVICES__;
    _eoka_patchMethod(MediaDevices.prototype, 'enumerateDevices', (original) => async function(...args) {
        const devices = await original.apply(this, args);
        if (devices.length === 0) {
            return fakeDevices.map(d => {
                const device = _eoka_instance(MediaDeviceInfo.prototype, {
                    deviceId: d.deviceId,
                    groupId: d.groupId,
                    kind: d.kind,
                    label: d.label
                });
                return Object.defineProperty(device, 'toJSON', {
                    value: _eoka_mark({ toJSON() { return d; } }.toJSON)
                });
            });
        }
        return devices;
    });
}
