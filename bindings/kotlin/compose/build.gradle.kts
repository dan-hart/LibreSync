plugins { id("com.android.library"); id("maven-publish"); id("org.jetbrains.kotlin.plugin.compose") }
group="io.libresync"
version="0.7.0"
android { publishing {singleVariant("release"){withSourcesJar()}}; namespace="io.libresync.compose"; compileSdk {version=release(37){minorApiLevel=0}}; defaultConfig {minSdk=24}; buildFeatures {compose=true}; compileOptions {sourceCompatibility=JavaVersion.VERSION_17;targetCompatibility=JavaVersion.VERSION_17} }
dependencies {api(project(":")); api(platform("androidx.compose:compose-bom:2025.10.00"));api("androidx.compose.material3:material3");implementation("androidx.activity:activity-compose:1.11.0");implementation("com.google.zxing:core:3.5.3")}

afterEvaluate {publishing {publications {create<MavenPublication>("release"){from(components["release"]);artifactId="libresync-compose";pom {licenses {license {name="GNU Affero General Public License v3.0";url="https://www.gnu.org/licenses/agpl-3.0.html"}};url="https://github.com/dan-hart/LibreSync"}}};repositories {maven{url=uri(rootProject.layout.buildDirectory.dir("repository"))}}}}
