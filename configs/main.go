package configs

import (
	"os"
	"os/user"
	"path"
	"path/filepath"
	"reflect"

	"github.com/spf13/viper"
)

type Config struct {
	viper      *viper.Viper
	configPath string
}

type Configs struct {
	projectConfigs *Config
	userConfigs    *Config
}

func unmarshalConfig(config *Config, data interface{}) error {
	err := config.viper.ReadInConfig()
	if err != nil {
		return err
	}
	return config.viper.Unmarshal(&data)
}

func marshalConfig(config *Config, cfg interface{}) error {
	reflectCfg := reflect.ValueOf(cfg)
	for i := 0; i < reflectCfg.NumField(); i++ {
		k := reflectCfg.Type().Field(i).Name
		v := reflectCfg.Field(i).Interface()
		config.viper.Set(k, v)
	}

	err := CreatePathIfNotExist(config.configPath)
	if err != nil {
		return err
	}

	err = config.viper.WriteConfig()

	return err
}

func New() *Configs {
	// Configs stored in projects (<project>/.railway)
	// Includes projectId, environmentId, etc
	projectDir, err := filepath.Abs("./.railway")
	if err != nil {
		panic(err)
	}
	projectViper := viper.New()

	projectPath := path.Join(projectDir, "./config.json")
	projectViper.SetConfigFile(projectPath)
	projectViper.ReadInConfig()
	user, err := user.Current()
	if err != nil {
		panic(err)
	}

	projectConfig := &Config{
		viper:      projectViper,
		configPath: projectPath,
	}

	// Configs stored in root (~/.railway)
	// Includes token, etc
	userViper := viper.New()
	userPath := path.Join(user.HomeDir, ".railway/config.json")
	userViper.SetConfigFile(userPath)
	userViper.ReadInConfig()

	userConfig := &Config{
		viper:      userViper,
		configPath: userPath,
	}

	return &Configs{
		projectConfigs: projectConfig,
		userConfigs:    userConfig,
	}
}

func IsDevMode() bool {
	environment, exists := os.LookupEnv("RAILWAY_ENV")
	return exists && environment == "develop"
}

func CreatePathIfNotExist(path string) error {
	dir := filepath.Dir(path)

	if _, err := os.Stat(dir); os.IsNotExist(err) {
		err = os.MkdirAll(dir, os.ModePerm)
		if err != nil {
			return err
		}
	}

	return nil
}
