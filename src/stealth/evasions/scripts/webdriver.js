
// `--disable-blink-features=AutomationControlled` already makes the native
// getter report false. Only replace it where the browser still reports true.
if (navigator.webdriver) {
    _eoka_patchGetter(Navigator.prototype, 'webdriver', () => function() { return false; });
}

// Remove automation-related properties from window
const automationProps = [
    'callPhantom', '_phantom', 'phantom', '__nightmare', 'domAutomation',
    'domAutomationController', '_selenium', '_Selenium_IDE_Recorder',
    'callSelenium', '__webdriver_script_fn', '__driver_evaluate',
    '__webdriver_evaluate', '__fxdriver_evaluate', '__driver_unwrapped',
    '__webdriver_unwrapped', '__fxdriver_unwrapped', '__selenium_unwrapped',
    '_WEBDRIVER_ELEM_CACHE', 'ChromeDriverw', 'driver-evaluate',
    'webdriver-evaluate', 'selenium-evaluate', 'webdriverCommand',
    'webdriver-evaluate-response', '__webdriverFunc', '__lastWatirAlert',
    '__lastWatirConfirm', '__lastWatirPrompt', '$chrome_asyncScriptInfo',
    '__selenium_evaluate', '__webdriver_script_function'
];

automationProps.forEach(prop => {
    try {
        if (prop in window) delete window[prop];
    } catch(e) {}
});
