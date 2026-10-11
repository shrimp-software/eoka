
if (typeof speechSynthesis !== 'undefined') {
    const defaultVoices = __EOKA_SPEECH_VOICES__.map(v => {
        const voice = Object.create(SpeechSynthesisVoice.prototype);
        Object.defineProperties(voice, {
            name: { value: v.name, enumerable: true },
            lang: { value: v.lang, enumerable: true },
            localService: { value: v.localService, enumerable: true },
            default: { value: v.default, enumerable: true },
            voiceURI: { value: v.voiceURI, enumerable: true }
        });
        return voice;
    });

    const origGetVoices = speechSynthesis.getVoices.bind(speechSynthesis);
    speechSynthesis.getVoices = _eoka_mark(function getVoices() {
        const voices = origGetVoices();
        return voices.length > 0 ? voices : defaultVoices;
    }, 'getVoices');
}
