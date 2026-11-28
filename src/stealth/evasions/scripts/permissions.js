
// Stock Chrome (headless/new included) reports Notification.permission
// "default" and a native PermissionStatus "prompt". Only an inconsistent
// browser (legacy headless: "denied" without a user decision) is repaired,
// and the page keeps receiving real PermissionStatus objects.
if (typeof Notification !== 'undefined' && Notification.permission === 'denied' &&
        typeof Permissions !== 'undefined' && typeof PermissionStatus !== 'undefined') {
    const _eoka_promptStatuses = new WeakSet();
    _eoka_patchGetter(Notification, 'permission', () => function() { return 'default'; });
    _eoka_patchGetter(PermissionStatus.prototype, 'state', (original) => function() {
        return _eoka_promptStatuses.has(this) ? 'prompt' : original.call(this);
    });
    _eoka_patchMethod(Permissions.prototype, 'query', (original) => function(...args) {
        return original.apply(this, args).then((status) => {
            if (status && status.name === 'notifications') _eoka_promptStatuses.add(status);
            return status;
        });
    });
}
