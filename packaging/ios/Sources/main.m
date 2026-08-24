// squallar iOS entry shim.
//
// The real entry point is in the Rust staticlib (the squallar crate):
// `squallar_ios_main` hands control to the shared winit GUI loop, whose UIKit
// backend calls `UIApplicationMain` internally and never returns. So `main()`
// here is only a trampoline — there is no AppDelegate and no storyboard; winit
// creates the UIWindow and view controller itself.
extern int squallar_ios_main(void);

int main(int argc, char *argv[]) {
    (void)argc;
    (void)argv;
    return squallar_ios_main();
}
