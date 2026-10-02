plugins { id("com.android.library") version "9.1.1"; id("com.android.application") version "9.1.1" apply false; id("org.jetbrains.kotlin.plugin.compose") version "2.2.10" apply false; id("org.jetbrains.kotlin.plugin.serialization") version "2.2.10"; id("maven-publish") }
group="io.libresync"
version="0.7.0"
android {
 namespace="io.libresync.sdk"
 compileSdk { version=release(37) { minorApiLevel=0 } }
 buildToolsVersion="37.0.0"
 defaultConfig { minSdk=24; testInstrumentationRunner="androidx.test.runner.AndroidJUnitRunner"; consumerProguardFiles("consumer-rules.pro") }
 testOptions { targetSdk=37 }
 compileOptions { sourceCompatibility=JavaVersion.VERSION_17; targetCompatibility=JavaVersion.VERSION_17 }
 packaging { jniLibs.useLegacyPackaging=false }
 publishing { singleVariant("release") { withSourcesJar() } }
}
dependencies {
 api("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.10.2")
 api("org.jetbrains.kotlinx:kotlinx-serialization-json:1.9.0")
 api("androidx.lifecycle:lifecycle-common:2.9.4")
 androidTestImplementation("androidx.lifecycle:lifecycle-runtime:2.9.4")
 androidTestImplementation("androidx.test:runner:1.7.0")
 androidTestImplementation("androidx.test.ext:junit:1.3.0")
}
afterEvaluate { publishing { publications { create<MavenPublication>("release") { from(components["release"]); artifactId="libresync";pom {licenses {license {name="GNU Affero General Public License v3.0";url="https://www.gnu.org/licenses/agpl-3.0.html"}};url="https://github.com/dan-hart/LibreSync"} } }; repositories { maven { url=uri(layout.buildDirectory.dir("repository")) } } } }
