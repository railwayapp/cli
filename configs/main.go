package configs

import (
	"os"
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
	projectConfigs     *Config
	userConfigs        *Config
	RailwayEnvToken    string
	RailwayEnvFilePath string
}

func IsDevMode() bool {
	environment, exists := os.LookupEnv("RAILWAY_ENV")
	return exists && environment == "develop"
}

func (c *Configs) CreatePathIfNotExist(path string) error {
	dir := filepath.Dir(path)

	if _, err := os.Stat(dir); os.IsNotExist(err) {
		err = os.MkdirAll(dir, os.ModePerm)
		if err != nil {
			return err
		}
	}

	return nil
}

func (c *Configs) unmarshalConfig(config *Config, data interface{}) error {
	err := config.viper.ReadInConfig()
	if err != nil {
		return err
	}
	return config.viper.Unmarshal(&data)
}

func (c *Configs) marshalConfig(config *Config, cfg interface{}) error {
	reflectCfg := reflect.ValueOf(cfg)
	for i := 0; i < reflectCfg.NumField(); i++ {
		k := reflectCfg.Type().Field(i).Name
		v := reflectCfg.Field(i).Interface()
		config.viper.Set(k, v)
	}

	err := c.CreatePathIfNotExist(config.configPath)
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
	userPath := path.Join(os.Getenv("HOME"), ".railway/config.json")
	userViper.SetConfigFile(userPath)
	userViper.ReadInConfig()

	userConfig := &Config{
		viper:      userViper,
		configPath: userPath,
	}

	return &Configs{
		projectConfigs:     projectConfig,
		userConfigs:        userConfig,
		RailwayEnvToken:    os.Getenv("RAILWAY_TOKEN"),
		RailwayEnvFilePath: path.Join(projectDir, "env.json"),
	}
}
