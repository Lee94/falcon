buildscript {
    dependencies {
        // AGP 9 的内置 Kotlin 默认用内嵌 KGP（9.5.x 内嵌 2.2.10），这里把它升到 2.4.21
        classpath("org.jetbrains.kotlin:kotlin-gradle-plugin:2.4.21")
    }
}

plugins {
    id("com.android.application") version "9.5.1" apply false
    // AGP 9 内置 Kotlin 支持，不再需要 org.jetbrains.kotlin.android；
    // Compose 编译器插件仍要单独声明，版本与 KGP 对齐
    id("org.jetbrains.kotlin.plugin.compose") version "2.4.21" apply false
}
