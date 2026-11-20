plugins {
    id("com.android.library")
}

android {
    namespace = "com.google.vtfx.session"
    compileSdk = 37
    defaultConfig { minSdk = 29 }
}

dependencies {
    api(project(":vtfx-core"))
    api(project(":vtfx-gate"))
    implementation(project(":vtfx-input"))
}
