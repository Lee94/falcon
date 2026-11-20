import java.io.File

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
}

android {
    namespace = "com.falcon.android"
    // compose-bom 2026.09.00、core-ktx 1.19.1、lifecycle 2.11.0、okhttp 5.5.1 的 AAR 元数据
    // 要求 compileSdk ≥ 37，低于它 checkReleaseAarMetadata 直接失败；targetSdk 与它分开升，仍是 36
    compileSdk = 37

    defaultConfig {
        applicationId = "com.falcon.android"
        minSdk = 29
        targetSdk = 36
        versionCode = 414
        versionName = "0.4.14"

        ndk {
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
    }

    val keystoreFilePath = System.getenv("ANDROID_KEYSTORE_FILE")?.takeIf { it.isNotBlank() }
    val releaseSigningConfig = if (keystoreFilePath != null) {
        val keystoreFile = File(keystoreFilePath).let {
            if (it.isAbsolute) it else rootProject.file(keystoreFilePath)
        }
        require(keystoreFile.isFile) { "ANDROID_KEYSTORE_FILE 不存在: $keystoreFilePath" }
        val storePasswordValue = System.getenv("ANDROID_KEYSTORE_PASSWORD")?.takeIf { it.isNotBlank() }
            ?: error("已设置 ANDROID_KEYSTORE_FILE，但缺少 ANDROID_KEYSTORE_PASSWORD")
        val keyAliasValue = System.getenv("ANDROID_KEY_ALIAS")?.takeIf { it.isNotBlank() }
            ?: error("已设置 ANDROID_KEYSTORE_FILE，但缺少 ANDROID_KEY_ALIAS")
        val keyPasswordValue = System.getenv("ANDROID_KEY_PASSWORD")?.takeIf { it.isNotBlank() }
            ?: storePasswordValue
        signingConfigs.create("release") {
            storeFile = keystoreFile
            storePassword = storePasswordValue
            keyAlias = keyAliasValue
            keyPassword = keyPasswordValue
        }
    } else {
        signingConfigs.getByName("debug")
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )
            // 已配置环境变量时使用固定发布证书（保证跨版本覆盖安装），否则回退 debug 签名
            signingConfig = releaseSigningConfig
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlin {
        compilerOptions {
            jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
        }
    }

    buildFeatures {
        compose = true
    }

    // 与桌面共用文案源，Kotlin 直接读取同一份语言文件，不维护第二份升级文案。
    sourceSets.getByName("main").assets.srcDir("../../native/crates/falcon-ui/locales")

    packaging {
        jniLibs {
            useLegacyPackaging = false
        }
    }
}

dependencies {
    implementation(project(":vtfx-compose"))
    val composeBom = platform("androidx.compose:compose-bom:2026.09.00")
    implementation(composeBom)
    implementation("androidx.core:core-ktx:1.19.1")
    implementation("androidx.lifecycle:lifecycle-runtime-ktx:2.11.0")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.11.0")
    implementation("androidx.activity:activity-compose:1.13.0")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-graphics")
    implementation("androidx.compose.ui:ui-tooling-preview")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-extended")
    implementation("com.squareup.okhttp3:okhttp:5.5.1")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.11.0")
    testImplementation("junit:junit:4.13.2")
    testImplementation("org.json:json:20240303")
    testImplementation("com.squareup.okhttp3:mockwebserver:5.5.1")
}
