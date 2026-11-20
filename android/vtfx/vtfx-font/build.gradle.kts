plugins {
    id("com.android.library")
}

android {
    namespace = "com.google.vtfx.font"
    compileSdk = 37
    defaultConfig { minSdk = 29 }
}

dependencies {
    api(project(":vtfx-font-gate"))
}
