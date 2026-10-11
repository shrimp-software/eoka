
if (navigator.mediaDevices && navigator.mediaDevices.enumerateDevices) {
    const origEnumerateDevices = navigator.mediaDevices.enumerateDevices.bind(navigator.mediaDevices);

    const fakeDevices = __EOKA_MEDIA_DEVICES__;
    navigator.mediaDevices.enumerateDevices = _eoka_mark(async function enumerateDevices() {
        const devices = await origEnumerateDevices();
        if (devices.length === 0) {
            return fakeDevices.map(d => {
                const device = Object.create(MediaDeviceInfo.prototype);
                Object.defineProperties(device, {
                    deviceId: { value: d.deviceId, enumerable: true },
                    groupId: { value: d.groupId, enumerable: true },
                    kind: { value: d.kind, enumerable: true },
                    label: { value: d.label, enumerable: true },
                    toJSON: { value: _eoka_mark(() => d, 'toJSON') }
                });
                return device;
            });
        }
        return devices;
    }, 'enumerateDevices');
}
