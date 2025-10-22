import init, * as wasm from './web-pack/rustdar_platform_lib.js';

async function initializeEmulator() {
    try {
        console.log('Initializing WASM module...');
        await init();
        console.log('WASM module loaded successfully');
    } catch (error) {
        console.error('Failed to initialize emulator:', error);
    }
}

// Initialize everything when the page loads
window.addEventListener('DOMContentLoaded', () => {
    console.log('DOM loaded, setting up...');

    // Start initializing the emulator immediately
    initializeEmulator().catch(error => {
        console.error('Failed to start emulator:', error);
    });
});

// Prevent context menu on right-click (common for games)
document.addEventListener('contextmenu', (event) => {
    event.preventDefault();
});

console.log('JavaScript WASM loader initialized');
