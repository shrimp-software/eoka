
if (typeof MediaDevices !== 'undefined' && MediaDevices.prototype.enumerateDevices) {
    const fakeDevices = __EOKA_MEDIA_DEVICES__;
    _eoka_patchMethod(MediaDevices.prototype, 'enumerateDevices', (original) => async function(...args) {
        const devices = await original.apply(this, args);
        if (devices.length === 0) {
            return fakeDevices.map(d => {
                const device = Object.create(MediaDeviceInfo.prototype);
                Object.defineProperties(device, {
                    deviceId: { value: d.deviceId, enumerable: true },
                    groupId: { value: d.groupId, enumerable: true },
                    kind: { value: d.kind, enumerable: true },
                    label: { value: d.label, enumerable: true },
                    toJSON: { value: _eoka_mark({ toJSON() { return d; } }.toJSON) }
                });
                return device;
            });
        }
        return devices;
    });
}
