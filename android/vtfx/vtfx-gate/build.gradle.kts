plugins {
    id("com.android.library")
}

android {
    namespace = "com.google.vtfx.gate"
    compileSdk = 37
    defaultConfig { minSdk = 29 }
}

dependencies {
    implementation(project(":vtfx-core"))
}
