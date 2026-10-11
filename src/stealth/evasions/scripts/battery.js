
const originalGetBattery = Navigator.prototype.getBattery;
if (originalGetBattery) {
    Object.defineProperty(Navigator.prototype, 'getBattery', {
        value: _eoka_mark(function getBattery() {
            return originalGetBattery.call(this).then(function(battery) {
                return new Proxy(battery, {
                    get(target, prop) {
                        switch (prop) {
                            case 'charging': return true;
                            case 'chargingTime': return 0;
                            case 'dischargingTime': return Infinity;
                            case 'level': return 1;
                        }
                        const v = target[prop];
                        return typeof v === 'function' ? v.bind(target) : v;
                    }
                });
            });
        }, 'getBattery'),
        configurable: true,
        writable: true
    });
}
